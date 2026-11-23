//! Эквалайзер из «Настроек звука»: предусиление и 10 пиковых фильтров ±12 дБ.

use rodio::source::SeekError;
use rodio::Source;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_Q: f32 = 1.4;
pub const FREQS: [f32; 10] = [60.0, 170.0, 310.0, 600.0, 1000.0, 3000.0, 6000.0, 12000.0, 14000.0, 16000.0];

/// Пресеты в порядке выпадающего списка. Первый — «Своя настройка».
pub const PRESETS: [(&str, [f32; 10]); 19] = [
    ("Своя настройка", [0.0; 10]),
    ("По умолчанию", [0.0; 10]),
    ("Классическая музыка", [-0.5, -0.5, -0.5, -0.5, -0.5, -0.5, -3.5, -3.5, -3.5, -4.5]),
    ("Клубная музыка", [-0.5, -0.5, 4.0, 2.5, 2.5, 2.5, 1.5, -0.5, -0.5, -0.5]),
    ("Танцевальная музыка", [4.5, 3.5, 1.0, -0.5, -0.5, -2.5, -3.5, -3.5, -0.5, -0.5]),
    ("Усиление НЧ", [4.0, 4.5, 4.5, 2.5, 0.5, -2.0, -4.0, -5.0, -5.5, -5.5]),
    ("Усиление НЧ и ВЧ", [3.5, 2.5, -0.5, -3.5, -2.0, 0.5, 4.0, 5.5, 6.0, 6.0]),
    ("Усиление ВЧ", [-4.5, -4.5, -4.5, -2.0, 1.0, 5.5, 8.0, 8.0, 8.0, 8.0]),
    ("Колонки ноутбука", [2.0, 5.5, 2.5, -1.5, -1.0, 0.5, 2.0, 4.5, 6.0, 7.0]),
    ("Большой зал", [5.0, 5.0, 2.5, 2.5, -0.5, -2.0, -2.0, -2.0, -0.5, -0.5]),
    ("Концерт", [-2.0, -0.5, 2.0, 2.5, 2.5, 2.5, 2.0, 1.0, 1.0, 1.0]),
    ("Вечеринка", [3.5, 3.5, -0.5, -0.5, -0.5, -0.5, -0.5, -0.5, 3.5, 3.5]),
    ("Поп", [-0.5, 2.0, 3.5, 4.0, 2.5, -0.5, -1.0, -1.0, -0.5, -0.5]),
    ("Регги", [-0.5, -0.5, -0.5, -2.5, -0.5, 3.0, 3.0, -0.5, -0.5, -0.5]),
    ("Рок", [4.0, 2.0, -2.5, -4.0, -1.5, 2.0, 4.0, 5.5, 5.5, 5.5]),
    ("Ска", [-1.0, -2.0, -2.0, -0.5, 2.0, 2.5, 4.0, 4.5, 5.5, 4.5]),
    ("Мягкое звучание", [2.0, 0.5, -0.5, -1.0, -0.5, 2.0, 4.0, 4.5, 5.5, 6.0]),
    ("Софт-рок", [2.0, 2.0, 1.0, -0.5, -2.0, -2.5, -1.5, -0.5, 1.0, 4.0]),
    ("Техно", [4.0, 2.5, -0.5, -2.5, -2.0, -0.5, 4.0, 4.5, 4.5, 4.0]),
];

/// Настройки, общие для UI и аудиопотока.
pub struct EqParams {
    #[cfg(target_os = "macos")]
    pub(crate) spectrum: Arc<crate::spectrum::Spectrum>,
    on: AtomicBool,
    preamp: AtomicU32,
    gains: [AtomicU32; 10],
    frequencies: [AtomicU32; 10],
    q: [AtomicU32; 10],
    version: AtomicU64,
}

impl Default for EqParams {
    fn default() -> Self {
        EqParams {
            #[cfg(target_os = "macos")]
            spectrum: Default::default(),
            on: AtomicBool::new(false),
            preamp: AtomicU32::new(1.0f32.to_bits()),
            gains: Default::default(),
            frequencies: FREQS.map(|freq| AtomicU32::new(freq.to_bits())),
            q: std::array::from_fn(|_| AtomicU32::new(DEFAULT_Q.to_bits())),
            version: AtomicU64::new(1),
        }
    }
}

impl EqParams {
    /// `preamp` — множитель громкости 0,5…1,5, `gains` — дБ по полосам.
    pub fn set(&self, on: bool, preamp: f32, gains: &[f32; 10], frequencies: &[f32; 10], q: &[f32; 10]) {
        self.on.store(on, Ordering::Relaxed);
        self.preamp.store(preamp.to_bits(), Ordering::Relaxed);
        for i in 0..10 {
            self.gains[i].store(gains[i].clamp(-12.0, 12.0).to_bits(), Ordering::Relaxed);
            self.frequencies[i].store(frequencies[i].clamp(20.0, 20_000.0).to_bits(), Ordering::Relaxed);
            self.q[i].store(q[i].clamp(0.3, 8.0).to_bits(), Ordering::Relaxed);
        }
        self.version.fetch_add(1, Ordering::Release);
    }
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    fn peaking(freq: f32, gain_db: f32, q: f32, rate: f32) -> Biquad {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * (freq.min(rate * 0.45)) / rate;
        let alpha = w0.sin() / (2.0 * q);
        let cos = w0.cos();
        let a0 = 1.0 + alpha / a;
        Biquad {
            b0: (1.0 + alpha * a) / a0,
            b1: -2.0 * cos / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }
}

struct Echo {
    delay: Vec<f32>,
    rate: usize,
    channels: usize,
    cursor: usize,
    filled: usize,
    length: usize,
    milliseconds: u16,
}

impl Echo {
    fn new(rate: u32, channels: u16) -> Self {
        let rate = rate as usize;
        let channels = usize::from(channels);
        let capacity = (rate * 511 / 1000 + 1) * channels;
        Self { delay: if channels <= 2 { vec![0.0; capacity] } else { Vec::new() }, rate, channels,
            cursor: 0, filled: 0, length: 0, milliseconds: 0 }
    }

    fn tap(&mut self, signal: f32, ms: u16, send: f32) -> f32 {
        if self.delay.is_empty() { return 0.0; }
        if self.milliseconds != ms {
            self.milliseconds = ms;
            self.length = ((self.rate * usize::from(ms) / 1000).max(1) * self.channels).min(self.delay.len());
            self.cursor = 0;
            self.filled = 0;
        }
        let repeat = if self.filled == self.length { self.delay[self.cursor] } else { 0.0 };
        self.delay[self.cursor] = signal * send + repeat * 0.34;
        self.cursor += 1;
        if self.cursor == self.length { self.cursor = 0; }
        self.filled = (self.filled + 1).min(self.length);
        repeat
    }
}

pub struct Eq<S> {
    inner: S,
    params: Arc<EqParams>,
    version: u64,
    on: bool,
    preamp: f32,
    bands: Vec<(usize, Biquad)>,
    /// Состояние фильтров: [канал][полоса] = (x1, x2, y1, y2).
    state: Vec<[[f32; 4]; 10]>,
    ch: usize,
    count: u32,
    fade: crate::crossfade::Envelope,
    frame_gain: f32,
    mix_low: [f32; 8],
    mix_alpha: f32,
    mix_upper: [f32; 8],
    upper_alpha: f32,
    echo: Echo,
    #[cfg(target_os = "macos")]
    mono: f32,
}

impl<S: Source<Item = f32>> Eq<S> {
    pub fn new(inner: S, params: Arc<EqParams>, control: crate::crossfade::Control) -> Self {
        let fade = crate::crossfade::Envelope::new(control, inner.sample_rate());
        let mix_alpha = 1.0 - (-std::f32::consts::TAU * 160.0 / inner.sample_rate().max(1) as f32).exp();
        let upper_alpha = 1.0 - (-std::f32::consts::TAU * 2600.0 / inner.sample_rate().max(1) as f32).exp();
        let echo = Echo::new(inner.sample_rate(), inner.channels());
        let mut eq = Eq { inner, params, version: 0, on: false, preamp: 1.0, bands: Vec::with_capacity(10), state: Vec::new(), ch: 0, count: 0, fade, frame_gain: 1.0,
            mix_low: [0.0; 8], mix_alpha, mix_upper: [0.0; 8], upper_alpha, echo,
            #[cfg(target_os = "macos")]
            mono: 0.0,
        };
        #[cfg(target_os = "macos")]
        eq.params.spectrum.set_rate(eq.inner.sample_rate());
        eq.refresh();
        eq
    }

    fn refresh(&mut self) {
        let v = self.params.version.load(Ordering::Acquire);
        if v == self.version {
            return;
        }
        self.version = v;
        self.on = self.params.on.load(Ordering::Relaxed);
        self.preamp = f32::from_bits(self.params.preamp.load(Ordering::Relaxed));
        let rate = self.inner.sample_rate().max(8000) as f32;
        self.bands.clear();
        for i in 0..10 {
            let gain = f32::from_bits(self.params.gains[i].load(Ordering::Relaxed));
            if self.on && gain.abs() > 0.01 {
                let freq = f32::from_bits(self.params.frequencies[i].load(Ordering::Relaxed));
                let q = f32::from_bits(self.params.q[i].load(Ordering::Relaxed));
                self.bands.push((i, Biquad::peaking(freq, gain, q, rate)));
            } else {
                for channel in &mut self.state { channel[i] = [0.0; 4]; }
            }
        }
        let channels = self.inner.channels().max(1) as usize;
        if self.state.len() != channels {
            self.state = vec![[[0.0; 4]; 10]; channels];
        }
    }
}

impl<S: Source<Item = f32>> Iterator for Eq<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let x = self.inner.next()?;
        self.count += 1;
        if self.count >= 2048 && self.ch == 0 {
            self.count = 0;
            self.refresh();
        }
        let channels = self.state.len().max(1);
        let ch = self.ch % channels;
        self.ch = (self.ch + 1) % channels;
        let mut y = x;
        if self.on {
            y *= self.preamp;
            let st = &mut self.state[ch];
            for &(i, f) in &self.bands {
                let s = &mut st[i];
                let out = f.b0 * y + f.b1 * s[0] + f.b2 * s[1] - f.a1 * s[2] - f.a2 * s[3];
                s[1] = s[0];
                s[0] = y;
                s[3] = s[2];
                s[2] = out;
                y = out;
            }
            y = y.clamp(-1.0, 1.0);
        }
        if ch == 0 { self.frame_gain = self.fade.frame(); }
        let dry = y;
        y *= self.frame_gain;
        if ch < self.mix_low.len() && (self.fade.low_delta != 0.0 || self.fade.mid_delta != 0.0 || self.fade.echo_ms != 0) {
            self.mix_low[ch] += self.mix_alpha * (dry - self.mix_low[ch]);
            self.mix_upper[ch] += self.upper_alpha * (dry - self.mix_upper[ch]);
            y += self.fade.low_delta * self.mix_low[ch] + self.fade.mid_delta * self.mix_upper[ch];
            if self.fade.echo_ms > 0 {
                let band = self.mix_upper[ch] - self.mix_low[ch];
                y += self.echo.tap(band, self.fade.echo_ms, self.fade.echo_send) * self.fade.echo_wet;
            } else { self.echo.milliseconds = 0; }
        }
        y *= self.fade.headroom;
        #[cfg(target_os = "macos")]
        if self.params.spectrum.active.load(Ordering::Relaxed) {
            self.mono += y;
            if self.ch == 0 {
                self.params.spectrum.push(self.mono / channels as f32);
                self.mono = 0.0;
            }
        } else {
            self.mono = 0.0;
        }
        Some(y)
    }
}

impl<S: Source<Item = f32>> Source for Eq<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }

    fn channels(&self) -> u16 {
        self.inner.channels()
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.state.fill([[0.0; 4]; 10]);
        self.mix_low.fill(0.0);
        self.mix_upper.fill(0.0);
        self.echo.milliseconds = 0;
        #[cfg(target_os = "macos")]
        { self.mono = 0.0; }
        self.inner.try_seek(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;

    fn level(tone: f32, center: f32, q: f32, on: bool) -> f32 {
        let rate = 48000;
        let samples: Vec<f32> = (0..rate / 2).flat_map(|i| {
            let x = 0.05 * (i as f32 * tone * std::f32::consts::TAU / rate as f32).sin();
            [x, x]
        }).collect();
        let params = Arc::new(EqParams::default());
        let mut frequencies = FREQS;
        frequencies[4] = center;
        let mut gains = [0.0; 10];
        gains[4] = 6.0;
        params.set(on, 1.0, &gains, &frequencies, &[q; 10]);
        let audio = Eq::new(SamplesBuffer::new(2, rate, samples), params, Default::default());
        audio.skip(24000).map(|x| x * x).sum::<f32>().sqrt()
    }

    #[test]
    fn echo_repeats_on_the_selected_subdivision_and_keeps_stereo_separate() {
        let mut echo = Echo::new(48_000, 2);
        let mut taps = Vec::new();
        for frame in 0..=24_000 {
            let left = echo.tap(if frame == 0 { 1.0 } else { 0.0 }, 250, 1.0);
            let right = echo.tap(0.0, 250, 1.0);
            assert_eq!(right, 0.0);
            if left != 0.0 { taps.push((frame, left)); }
        }
        assert_eq!(taps, vec![(12_000, 1.0), (24_000, 0.34)]);
        echo.milliseconds = 0;
        assert_eq!(echo.tap(0.0, 250, 1.0), 0.0);
    }

    #[test]
    fn parametric_band_changes_center_and_bandwidth_and_bypasses_flat() {
        let reference = level(2000.0, 2000.0, 2.0, false);
        let peak = level(2000.0, 2000.0, 2.0, true);
        assert!((peak / reference - 2.0).abs() < 0.03);
        assert!(level(2000.0, 4000.0, 6.0, true) < reference * 1.05);
        assert!(level(1600.0, 2000.0, 0.5, true) > level(1600.0, 2000.0, 6.0, true) * 1.3);
    }
}
