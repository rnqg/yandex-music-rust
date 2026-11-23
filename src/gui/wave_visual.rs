use super::{from_hsl, hsl, App, AppWindow, ComponentHandle, Image};
use crate::assets::Asar;
use crate::spectrum::Spectrum;
use base64::{engine::general_purpose::STANDARD, Engine};
use slint::{GraphicsAPI, RenderingState};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

const CHUNKS: &str = "app/_next/static/chunks";
const WORKER_PREFIX: &str = "Lyogcm9sbHVwLXBsdWdpbi13ZWI";
const TEMPLATE_PREFIX: &str = r"`\nprecision highp float;\n\nuniform vec2 vScreenSize;";
const SIDE: u32 = 448;
type Palette = [[f32; 3]; 6];

#[derive(Clone, Copy, Default)]
struct Style {
    color: Option<[u8; 3]>,
    playing: bool,
    volume: f32,
    liked_at: Option<Instant>,
}

enum Renderer {
    New,
    Loading(std::sync::mpsc::Receiver<Result<Painter, String>>),
    Ready(Painter),
    Unavailable,
}

pub(super) struct WaveVisual {
    spectrum: Arc<Spectrum>,
    style: Cell<Style>,
    enabled: Cell<bool>,
}

impl WaveVisual {
    pub(super) fn attach(ui: &AppWindow, spectrum: Arc<Spectrum>) -> Rc<Self> {
        let state = Rc::new(Self { spectrum, style: Cell::default(), enabled: Cell::new(false) });
        let wave = state.clone();
        let window = ui.as_weak();
        let mut renderer = Renderer::New;
        let mut animation = Animation::new();
        let result = ui.window().set_rendering_notifier(move |stage, api| {
            let Some(ui) = window.upgrade() else { return };
            let app = ui.global::<App>();
            if matches!(stage, RenderingState::RenderingTeardown) {
                app.set_wave_frame(Image::default());
                renderer = Renderer::New;
                return;
            }
            if !matches!(stage, RenderingState::BeforeRendering) || !wave.enabled.get() {
                return;
            }
            let GraphicsAPI::WGPU30 { device, queue, .. } = api else { return };
            if matches!(renderer, Renderer::New) {
                let device = device.clone();
                let (send, receive) = std::sync::mpsc::sync_channel(1);
                let ready = ui.as_weak();
                std::thread::spawn(move || {
                    let result = source().ok_or_else(|| "не найден исходный шейдер".to_string())
                        .and_then(|source| Painter::new(&device, &source));
                    if send.send(result).is_ok() {
                        let _ = ready.upgrade_in_event_loop(|ui| ui.window().request_redraw());
                    }
                });
                renderer = Renderer::Loading(receive);
            }
            if let Renderer::Loading(receiver) = &renderer {
                match receiver.try_recv() {
                    Ok(Ok(painter)) => renderer = Renderer::Ready(painter),
                    Err(std::sync::mpsc::TryRecvError::Empty) => (),
                    result => {
                        if let Ok(Err(error)) = result {
                            crate::config::log(format!("Шейдер «Моей волны»: {error}"));
                        }
                        renderer = Renderer::Unavailable;
                        wave.spectrum.active.store(false, Ordering::Relaxed);
                    }
                }
            }
            if let Renderer::Ready(painter) = &renderer {
                let style = wave.style.get();
                wave.spectrum.active.store(style.playing, Ordering::Relaxed);
                let uniforms = animation.advance(style, &wave.spectrum);
                painter.draw(device, queue, &uniforms);
                if app.get_wave_frame().size().width == 0 {
                    app.set_wave_frame(Image::try_from(painter.texture.clone()).expect("RGBA8 wave texture"));
                }
                ui.window().request_redraw();
            }
        });
        if let Err(error) = result {
            crate::config::log(format!("Рендерер «Моей волны»: {error:?}"));
        }
        state
    }

    pub(super) fn show(&self, ui: &AppWindow, enabled: bool, playing: bool, color: Option<[u8; 3]>, volume: f32) {
        let app = ui.global::<App>();
        let enabled = enabled && !app.get_wave_fact_open();
        let liked_at = self.style.get().liked_at.filter(|at| enabled && at.elapsed() < Duration::from_millis(1650));
        self.style.set(Style { color, playing, volume, liked_at });
        if !enabled || !playing {
            self.spectrum.active.store(false, Ordering::Relaxed);
        }
        if self.enabled.replace(enabled) != enabled {
            if !enabled {
                app.set_wave_frame(Image::default());
            }
            ui.window().request_redraw();
        }
    }

    pub(super) fn like(&self) {
        if self.enabled.get() {
            self.style.set(Style { liked_at: Some(Instant::now()), ..self.style.get() });
        }
    }
}

struct Animation {
    previous: Instant,
    phase: f32,
    speed: f32,
    colors: Option<Palette>,
    audio: [f32; 3],
}

impl Animation {
    fn new() -> Self {
        Self { previous: Instant::now(), phase: (crate::config::now_secs() % 3600) as f32,
            speed: 0.2, colors: None, audio: [0.0; 3] }
    }

    fn advance(&mut self, style: Style, spectrum: &Spectrum) -> [[f32; 4]; 14] {
        let now = Instant::now();
        let elapsed = (now - self.previous).as_secs_f32().min(0.08);
        self.previous = now;
        let target = palette(style.color);
        let colors = self.colors.get_or_insert(target);
        let weight = 1.0 - (-4.0 * elapsed).exp();
        for (current, target) in colors.iter_mut().flatten().zip(target.into_iter().flatten()) {
            *current += (target - *current) * weight;
        }
        self.speed += (if style.playing { 0.64 } else { 0.2 } - self.speed) * weight;
        self.phase = (self.phase + elapsed * self.speed) % 86400.0;
        let target = if style.playing { spectrum.bands(style.volume * style.volume) } else { [0.0; 3] };
        for (current, sample) in self.audio.iter_mut().zip(target) {
            *current = (*current - 0.5 * elapsed).max(sample);
        }
        let reaction = style.liked_at.map_or([0.0; 3], |at| like_reaction(at.elapsed().as_secs_f32()));
        uniforms(self.phase, colors, &self.audio, &reaction)
    }
}

fn uniforms(phase: f32, colors: &Palette, audio: &[f32; 3], reaction: &[f32; 3]) -> [[f32; 4]; 14] {
    let mut values = [[0.0; 4]; 14];
    values[0] = [SIDE as f32, SIDE as f32, phase, 0.35];
    for (row, color) in values[2..8].iter_mut().zip(colors) {
        row[..3].copy_from_slice(color);
    }
    values[8] = [-0.3, 0.3, 0.2, 0.0];
    values[9] = [-0.3, -0.3, -0.2, 0.0];
    values[10] = [-0.3, -0.3, 0.2, 0.0];
    values[11][..3].copy_from_slice(audio);
    values[12][..3].copy_from_slice(reaction);
    values
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
    let mut shader = String::from("#version 450\nlayout(location=0) out vec4 waveColor;\n\
        layout(std140, set=0, binding=0) uniform Wave {\n\
        vec2 vScreenSize; float vTime; float vScale; vec4 vColorBackground;\n\
        vec3 vColor[6]; vec3 vRotation[3]; vec4 vAudio; vec4 vReact;\n\
        vec2 vInteractionPoint; float vInteraction; };\n");
    for line in glsl.lines().filter(|line| !line.trim_start().starts_with("uniform ")) {
        if line.contains("${(t=>t?\"color.rgb") {
            shader.push_str("color.rgb = mix(color.rgb, blobColor.rgb, blobColor.a); color.a = max(blobColor.a, color.a);");
        } else if line.contains("${(t=>t?\"gl_FragColor") {
            shader.push_str("waveColor = vec4(color.a > 0.0 ? min(color.rgb / color.a, vec3(1.0)) : vec3(0.0), color.a);");
        } else {
            shader.push_str(line);
        }
        shader.push('\n');
    }
    let shader = shader.replace("${lt(e)}", "vec4").replace("${t}", "3")
        .replace("gl_FragCoord.xy", "vec2(gl_FragCoord.x, vScreenSize.y - gl_FragCoord.y)");
    (!shader.contains("${")).then_some(shader)
}

struct Painter {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    group: wgpu::BindGroup,
}

impl Painter {
    fn new(device: &wgpu::Device, source: &str) -> Result<Self, String> {
        use wgpu::naga::{front::glsl, valid, ShaderStage};
        let module = glsl::Frontend::default().parse(&glsl::Options::from(ShaderStage::Fragment), source)
            .map_err(|e| e.emit_to_string(source))?;
        valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::empty())
            .validate(&module).map_err(|e| e.emit_to_string(source))?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let fragment = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Wave fragment"), source: wgpu::ShaderSource::Naga(std::borrow::Cow::Owned(module)),
        });
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Wave vertex"),
            source: wgpu::ShaderSource::Wgsl("@vertex fn main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
                let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
                return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
            }".into()),
        });
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("My Wave"), layout: None,
            vertex: wgpu::VertexState { module: &vertex, entry_point: Some("main"), buffers: &[], compilation_options: Default::default() },
            fragment: Some(wgpu::FragmentState { module: &fragment, entry_point: Some("main"),
                targets: &[Some(format.into())], compilation_options: Default::default() }),
            primitive: Default::default(), depth_stencil: None, multisample: Default::default(), multiview_mask: None, cache: None,
        });
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        if cfg!(test) { usage |= wgpu::TextureUsages::COPY_SRC; }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Wave frame"), size: wgpu::Extent3d { width: SIDE, height: SIDE, depth_or_array_layers: 1 },
            mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage, view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Wave uniforms"), size: std::mem::size_of::<[[f32; 4]; 14]>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None, layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
        });
        if let Some(error) = futures_lite::future::block_on(scope.pop()) {
            return Err(error.to_string());
        }
        Ok(Self { texture, view, pipeline, buffer, group })
    }

    fn draw(&self, device: &wgpu::Device, queue: &wgpu::Queue, uniforms: &[[f32; 4]; 14]) {
        let bytes = uniforms.map(|row| row.map(f32::to_ne_bytes));
        queue.write_buffer(&self.buffer, 0, bytes.as_flattened().as_flattened());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Wave"), color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view, depth_slice: None, resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })], ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
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
        let instance = wgpu::Instance::default();
        let adapter = futures_lite::future::block_on(instance.request_adapter(&Default::default())).unwrap();
        let (device, queue) = futures_lite::future::block_on(adapter.request_device(&Default::default())).unwrap();
        let painter = Painter::new(&device, &source).unwrap();
        let colors = palette(Some([91, 144, 158]));
        let render = |phase, audio, reaction| {
            painter.draw(&device, &queue, &uniforms(phase, &colors, &audio, &reaction));
            let output = device.create_buffer(&wgpu::BufferDescriptor {
                label: None, size: u64::from(SIDE * SIDE * 4),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false,
            });
            let mut encoder = device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(painter.texture.as_image_copy(), wgpu::TexelCopyBufferInfo {
                buffer: &output, layout: wgpu::TexelCopyBufferLayout {
                    offset: 0, bytes_per_row: Some(SIDE * 4), rows_per_image: None,
                },
            }, painter.texture.size());
            queue.submit([encoder.finish()]);
            let (send, receive) = std::sync::mpsc::channel();
            output.slice(..).map_async(wgpu::MapMode::Read, move |r| { send.send(r).unwrap(); });
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            receive.recv().unwrap().unwrap();
            let pixels = output.slice(..).get_mapped_range().unwrap().to_vec();
            pixels
        };
        let first = render(14.0, [0.0; 3], [0.0; 3]);
        assert!(first.as_chunks::<4>().0.iter().any(|p| p[3] >= 128));
        assert!(first.as_chunks::<4>().0.iter().any(|p| p[3] == 0));
        for next in [render(14.4, [0.0; 3], [0.0; 3]), render(14.0, [0.6, 0.4, 0.2], [0.0; 3]),
            render(14.0, [0.0; 3], like_reaction(0.4))] {
            assert!(first != next);
        }
    }
}
