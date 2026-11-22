use super::{from_hsl, hsl, App, AppWindow, ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode};
use crate::assets::Asar;
use crate::spectrum::Spectrum;
use base64::{engine::general_purpose::STANDARD, Engine};
use objc2::rc::{autoreleasepool, Retained};
use objc2_metal::{MTLCreateSystemDefaultDevice, MTLDevice};
use skia_safe::{gpu, AlphaType, BlendMode, ColorType, Data, ImageInfo, Paint, RuntimeEffect, Surface};
use std::cell::{Cell, OnceCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CHUNKS: &str = "app/_next/static/chunks";
const WORKER_PREFIX: &str = "Lyogcm9sbHVwLXBsdWdpbi13ZWI";
const TEMPLATE_PREFIX: &str = r"`\nprecision highp float;\n\nuniform vec2 vScreenSize;";
const SIDE: i32 = 650;
type Palette = [[f32; 3]; 6];

#[derive(Clone, Copy, Default)]
struct Style {
    color: Option<[u8; 3]>,
    playing: bool,
    volume: f32,
    liked_at: Option<Instant>,
}

pub(super) struct WaveVisual {
    spectrum: Arc<Spectrum>,
    style: Cell<Style>,
    timer: Timer,
    requests: OnceCell<SyncSender<Style>>,
}

impl WaveVisual {
    pub(super) fn new(spectrum: Arc<Spectrum>) -> Self {
        Self { spectrum, style: Cell::default(), timer: Timer::default(), requests: OnceCell::new() }
    }

    pub(super) fn show(self: &Rc<Self>, ui: &AppWindow, enabled: bool, playing: bool, color: Option<[u8; 3]>, volume: f32) {
        let app = ui.global::<App>();
        let liked_at = self.style.get().liked_at.filter(|like| enabled && like.elapsed() < Duration::from_millis(1650));
        self.style.set(Style { color, playing, volume, liked_at });
        self.spectrum.active.store(enabled && playing && self.timer.running(), Ordering::Relaxed);
        if !enabled {
            if self.timer.running() {
                self.timer.stop();
                app.set_wave_frame_enabled(false);
                app.set_wave_frame(Image::default());
            }
            return;
        }
        if self.timer.running() {
            return;
        }
        let sender = self.requests.get_or_init(|| Self::start_renderer(ui.as_weak(), self.spectrum.clone())).clone();
        if sender.try_send(self.style.get()).is_err() {
            return;
        }
        app.set_wave_frame_enabled(true);
        self.spectrum.active.store(playing, Ordering::Relaxed);
        let state = Rc::downgrade(self);
        let window = ui.as_weak();
        self.timer.start(TimerMode::Repeated, Duration::from_millis(40), move || {
            if let Some(state) = state.upgrade() {
                if let Err(std::sync::mpsc::TrySendError::Disconnected(_)) = sender.try_send(state.style.get()) {
                    state.timer.stop();
                    state.spectrum.active.store(false, Ordering::Relaxed);
                    if let Some(ui) = window.upgrade() {
                        let app = ui.global::<App>();
                        app.set_wave_frame_enabled(false);
                        app.set_wave_frame(Image::default());
                    }
                }
            }
        });
    }

    pub(super) fn like(&self) {
        if self.timer.running() {
            self.style.set(Style { liked_at: Some(Instant::now()), ..self.style.get() });
        }
    }

    fn start_renderer(window: slint::Weak<AppWindow>, spectrum: Arc<Spectrum>) -> SyncSender<Style> {
        let (sender, receiver) = sync_channel::<Style>(1);
        std::thread::spawn(move || {
            let Some(mut painter) = autoreleasepool(|_| Painter::open()) else { return };
            let mut previous = Instant::now();
            let mut phase = (crate::config::now_secs() % 3600) as f32;
            let mut speed = 0.2;
            let mut colors = None;
            let mut audio = [0.0_f32; 3];
            while let Ok(style) = receiver.recv() {
                let elapsed = previous.elapsed().as_secs_f32().min(0.08);
                previous = Instant::now();
                let target = palette(style.color);
                let colors = colors.get_or_insert(target);
                let weight = 1.0 - (-4.0 * elapsed).exp();
                for (current, target) in colors.iter_mut().flatten().zip(target.into_iter().flatten()) {
                    *current += (target - *current) * weight;
                }
                speed += (if style.playing { 0.64 } else { 0.2 } - speed) * weight;
                phase = (phase + elapsed * speed) % 86400.0;
                let target = if style.playing { spectrum.bands(style.volume * style.volume) } else { [0.0; 3] };
                for (current, sample) in audio.iter_mut().zip(target) {
                    *current = (*current - 0.02).max(sample);
                }
                let reaction = style.liked_at.map_or([0.0; 3], |at| like_reaction(at.elapsed().as_secs_f32()));
                let Some(pixels) = autoreleasepool(|_| painter.draw(phase, colors, &audio, &reaction)) else { break };
                if window.upgrade_in_event_loop(move |ui| {
                    let app = ui.global::<App>();
                    if app.get_wave_frame_enabled() {
                        app.set_wave_frame(Image::from_rgba8_premultiplied(pixels));
                    }
                }).is_err() {
                    break;
                }
            }
        });
        sender
    }
}

fn palette(color: Option<[u8; 3]>) -> Palette {
    let hues = color.map_or([(300.0, 100.0, 50.0), (330.0, 100.0, 50.0), (50.0, 100.0, 50.0)], |color| {
        let (h, s, l) = hsl(color);
        [(h, s, l), (h, (s + 15.0).min(100.0), (l + 5.0).min(100.0)), (h, (s + 15.0).min(100.0), (l + 10.0).min(100.0))]
    });
    let colors = hues.map(|(h, s, l)| {
        let color = from_hsl(h, s, l);
        [color.red() as f32 / 255.0, color.green() as f32 / 255.0, color.blue() as f32 / 255.0]
    });
    [colors[0], colors[1], colors[2], colors[0], colors[1], colors[2]]
}

fn like_reaction(elapsed: f32) -> [f32; 3] {
    [(0.0, 0.85), (0.1, 0.95), (0.15, 1.05)].map(|(start, stop)| {
        let progress = |from: f32| ((elapsed - from) / 0.6).clamp(0.0, 1.0);
        0.7 * (progress(start) - progress(stop))
    })
}

fn source() -> Option<String> {
    let mut asar = Asar::open()?;
    for file in asar.list(CHUNKS).into_iter().filter(|name| name.ends_with(".js")) {
        let path = format!("{CHUNKS}/{file}");
        let Some(prefix) = asar.read_prefix(&path, 16 * 1024) else { continue };
        if !prefix.windows(WORKER_PREFIX.len()).any(|bytes| bytes == WORKER_PREFIX.as_bytes()) {
            continue;
        }
        let chunk = String::from_utf8(asar.read(&path)?).ok()?;
        let encoded = &chunk[chunk.find(WORKER_PREFIX)?..].split_once('"')?.0;
        let worker = String::from_utf8(STANDARD.decode(encoded).ok()?).ok()?;
        let Some((_, template)) = worker.split_once(TEMPLATE_PREFIX) else { continue };
        let template = template.split_once('`')?.0;
        let glsl = format!("uniform vec2 vScreenSize;{template}").replace(r"\n", "\n");
        return shader_source(&glsl);
    }
    None
}

fn shader_source(glsl: &str) -> Option<String> {
    let mut shader = String::with_capacity(glsl.len());
    for line in glsl.lines() {
        if let Some(parts) = line.strip_prefix("#define ") {
            let (name, value) = parts.split_once(' ')?;
            shader.push_str(&format!("const float {name} = {value};\n"));
        } else if line.contains("${(t=>t?\"color.rgb") {
            shader.push_str("color.rgb = mix(color.rgb, blobColor.rgb, blobColor.a); color.a = max(blobColor.a, color.a);\n");
        } else if line.contains("${(t=>t?\"gl_FragColor") {
            shader.push_str("return half4(min(color.rgb, color.a), color.a);\n");
        } else {
            shader.push_str(line);
            shader.push('\n');
        }
    }
    let shader = shader.replace("${lt(e)}", "vec4").replace("${t}", "3")
        .replace("vec2", "float2").replace("vec3", "float3").replace("vec4", "float4")
        .replace("void main()", "half4 main(float2 coord)")
        .replace("gl_FragCoord.xy", "float2(coord.x, vScreenSize.y - coord.y)");
    (!shader.contains("${")).then_some(shader)
}

struct Painter {
    effect: RuntimeEffect,
    surface: Surface,
    info: ImageInfo,
    uniforms: Vec<u8>,
    _context: gpu::DirectContext,
}

impl Painter {
    fn open() -> Option<Self> {
        let effect = RuntimeEffect::make_for_shader(source()?, None).map_err(|error| {
            crate::config::log(format!("Шейдер «Моей волны»: {error}"));
        }).ok()?;
        let device = MTLCreateSystemDefaultDevice()?;
        let queue = device.newCommandQueue()?;
        let backend = unsafe { gpu::mtl::BackendContext::new(Retained::as_ptr(&device).cast(), Retained::as_ptr(&queue).cast()) };
        let mut context = gpu::direct_contexts::make_metal(&backend, None)?;
        let info = ImageInfo::new((SIDE, SIDE), ColorType::RGBA8888, AlphaType::Premul, None);
        let surface = gpu::surfaces::render_target(&mut context, gpu::Budgeted::Yes, &info, None,
            gpu::SurfaceOrigin::TopLeft, None, None, None)?;
        let uniforms = vec![0; effect.uniform_size()];
        Some(Self { effect, surface, info, uniforms, _context: context })
    }

    fn uniform(&mut self, name: &str, values: &[f32]) {
        if let Some(uniform) = self.effect.find_uniform(name).filter(|u| u.size_in_bytes() == std::mem::size_of_val(values)) {
            let offset = uniform.offset();
            for (bytes, value) in self.uniforms[offset..offset + uniform.size_in_bytes()].chunks_exact_mut(4).zip(values) {
                bytes.copy_from_slice(&value.to_ne_bytes());
            }
        }
    }

    fn draw(&mut self, phase: f32, colors: &Palette, audio: &[f32; 3], reaction: &[f32; 3]) -> Option<SharedPixelBuffer<Rgba8Pixel>> {
        self.uniform("vScreenSize", &[SIDE as f32, SIDE as f32]);
        self.uniform("vTime", &[phase]);
        self.uniform("vScale", &[0.35]);
        self.uniform("vColor", colors.as_flattened());
        self.uniform("vRotation", &[-0.3, 0.3, 0.2, -0.3, -0.3, -0.2, -0.3, -0.3, 0.2]);
        self.uniform("vAudio", audio);
        self.uniform("vReact", reaction);
        let shader = self.effect.make_shader(Data::new_copy(&self.uniforms), &[], None)?;
        let mut paint = Paint::default();
        paint.set_shader(shader).set_blend_mode(BlendMode::Src);
        self.surface.canvas().draw_paint(&paint);
        let mut pixels = SharedPixelBuffer::new(SIDE as u32, SIDE as u32);
        self.surface.read_pixels(&self.info, pixels.make_mut_bytes(), SIDE as usize * 4, (0, 0)).then_some(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_wave_shader_draws_distinct_frames() {
        let Some(source) = source() else {
            assert!(Asar::open().is_none(), "installed archive has no compatible wave shader");
            return;
        };
        RuntimeEffect::make_for_shader(source, None).unwrap();
        let mut painter = autoreleasepool(|_| Painter::open()).expect("Metal wave surface");
        let colors = palette(Some([91, 144, 158]));
        let first = autoreleasepool(|_| painter.draw(14.0, &colors, &[0.0; 3], &[0.0; 3])).unwrap();
        let second = autoreleasepool(|_| painter.draw(14.4, &colors, &[0.0; 3], &[0.0; 3])).unwrap();
        let with_sound = autoreleasepool(|_| painter.draw(14.0, &colors, &[0.6, 0.4, 0.2], &[0.0; 3])).unwrap();
        let liked = autoreleasepool(|_| painter.draw(14.0, &colors, &[0.0; 3], &like_reaction(0.4))).unwrap();
        assert!(first.as_slice().iter().any(|pixel| pixel.a >= 128));
        assert!(first.as_slice().iter().any(|pixel| pixel.a == 0));
        assert_ne!(first.as_bytes(), second.as_bytes());
        assert_ne!(first.as_bytes(), with_sound.as_bytes());
        assert_ne!(first.as_bytes(), liked.as_bytes());
    }
}
