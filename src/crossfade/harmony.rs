use symphonia::core::dsp::{complex::Complex, fft::Fft};

const SIZE: usize = 1024;

pub(crate) struct Harmony {
    frames: Vec<[f32; 12]>,
    hop: f32,
}

impl Harmony {
    pub fn read(samples: &[[f32; 2]], rate: f32) -> Self {
        let fft = Fft::new(SIZE);
        let window: Vec<_> = (0..SIZE).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / SIZE as f32).cos()).collect();
        let mut spectrum = vec![Complex::default(); SIZE];
        let mut magnitudes = vec![0.0; SIZE / 2];
        let mut frames = Vec::new();
        for block in samples.windows(SIZE).step_by(SIZE / 2) {
            let balance: f32 = block.iter().map(|p| p[1] * p[1] - p[0] * p[0]).sum();
            let ch = usize::from(balance > 0.0);
            for ((z, v), w) in spectrum.iter_mut().zip(block).zip(&window) {
                *z = Complex::new(v[ch] * w, 0.0);
            }
            fft.fft_inplace(&mut spectrum);
            for (m, z) in magnitudes.iter_mut().zip(&spectrum) { *m = (z.re * z.re + z.im * z.im).sqrt(); }
            let mut chroma = [0.0; 12];
            let first = (120.0 * SIZE as f32 / rate).ceil() as usize;
            let last = (1600.0 * SIZE as f32 / rate) as usize;
            let bins = &magnitudes[first..=last];
            let average = bins.iter().sum::<f32>() / bins.len() as f32;
            for k in first..=last {
                let (a,b,c) = (magnitudes[k-1], magnitudes[k], magnitudes[k+1]);
                if b < 0.05 || b < average * 4.0 || b <= a || b <= c { continue; }
                let offset = (0.5 * (a - c) / (a - 2.0 * b + c)).clamp(-0.5, 0.5);
                let hz = (k as f32 + offset) * rate / SIZE as f32;
                let note = (69.0 + 12.0 * (hz / 440.0).log2()).round() as usize;
                chroma[note % 12] += b;
            }
            frames.push(chroma);
        }
        Self { frames, hop: SIZE as f32 * 0.5 / rate }
    }

    fn profile(&self, from: f32, to: f32) -> Option<[f32; 12]> {
        let first = (from / self.hop - 1.0).round().max(0.0) as usize;
        let count = ((to - from) / self.hop).ceil().max(1.0) as usize;
        let mut chroma = [0.0; 12];
        let mut tonal = 0;
        let frames = self.frames.get(first..(first + count).min(self.frames.len()))?;
        for frame in frames {
            let sum: f32 = frame.iter().sum();
            if sum < 0.1 { continue; }
            tonal += 1;
            for (acc, v) in chroma.iter_mut().zip(frame) { *acc += v / sum; }
        }
        if tonal < 3 || tonal * 3 < frames.len() { return None; }
        for x in &mut chroma { *x /= tonal as f32; }
        (chroma.iter().copied().fold(0.0, f32::max) > 0.20).then_some(chroma)
    }

    pub fn clashes(&self, from: f32, to: f32, other: &Self, intro: f32, until: f32) -> Option<bool> {
        let left = self.profile(from, to)?;
        let right = other.profile(intro, until)?;
        let mut tension = 0.0;
        let mut consonance = 0.0;
        for i in 0..12 {
            tension += left[i] * (right[(i+1)%12] + right[(i+11)%12] + 0.65 * right[(i+6)%12]);
            consonance += left[i] * (right[i] + 0.65 * (right[(i+5)%12] + right[(i+7)%12]));
        }
        Some(tension > 0.17 && tension > consonance * 1.45)
    }
}
