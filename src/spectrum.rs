use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;

const FFT_SIZE: usize = 32;
const BINS: usize = FFT_SIZE / 2;
const RING_SIZE: usize = FFT_SIZE * 4;
const BANDS: [(f32, f32); 3] = [(0.0, 250.0), (500.0, 2000.0), (2000.0, 4000.0)];
type Coefficients = [[[f32; 2]; FFT_SIZE]; BINS];

pub(crate) struct Spectrum {
    pub(crate) active: AtomicBool,
    rate: AtomicU32,
    samples: [AtomicU32; RING_SIZE],
    next: AtomicUsize,
}

impl Default for Spectrum {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            rate: AtomicU32::new(48_000),
            samples: std::array::from_fn(|_| AtomicU32::new(0)),
            next: AtomicUsize::new(0),
        }
    }
}

impl Spectrum {
    pub(crate) fn set_rate(&self, rate: u32) {
        self.rate.store(rate.max(1), Ordering::Relaxed);
    }

    pub(crate) fn push(&self, sample: f32) {
        let index = self.next.load(Ordering::Relaxed);
        self.samples[index % RING_SIZE].store(sample.to_bits(), Ordering::Relaxed);
        self.next.store(index.wrapping_add(1), Ordering::Release);
    }

    pub(crate) fn bands(&self, gain: f32) -> [f32; 3] {
        static TABLE: OnceLock<Coefficients> = OnceLock::new();
        let table = TABLE.get_or_init(|| std::array::from_fn(|bin| std::array::from_fn(|index| {
            let position = TAU * index as f32 / FFT_SIZE as f32;
            let window = (0.42 - 0.5 * position.cos() + 0.08 * (2.0 * position).cos()) / FFT_SIZE as f32;
            let (sin, cos) = (position * bin as f32).sin_cos();
            [cos * window, sin * window]
        })));
        let end = self.next.load(Ordering::Acquire);
        let samples: [f32; FFT_SIZE] = std::array::from_fn(|index| {
            let index = end.wrapping_sub(FFT_SIZE).wrapping_add(index);
            f32::from_bits(self.samples[index % RING_SIZE].load(Ordering::Relaxed)) * gain
        });
        let bin_width = self.rate.load(Ordering::Relaxed) as f32 / BINS as f32;
        let ranges = BANDS.map(|(low, high)| ((low / bin_width) as usize, (high / bin_width) as usize));
        let mut prefix = [0.0; BINS + 1];
        for (index, kernel) in table.iter().take(ranges[2].1.saturating_add(1)).enumerate() {
            let [real, imaginary] = samples.iter().zip(kernel).fold([0.0, 0.0], |[real, imaginary], (sample, [cos, sin])| {
                [real + sample * cos, imaginary + sample * sin]
            });
            let db = 20.0 * real.hypot(imaginary).log10();
            let level = ((db + 100.0) / 70.0 * 255.0).clamp(0.0, 255.0).floor() / 256.0;
            prefix[index + 1] = prefix[index] + level;
        }
        ranges.map(|(first, last)| {
            (prefix[(last + 1).min(BINS)] - prefix[first.min(BINS)]) / (last - first + 1) as f32
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_spectrum_uses_latest_samples_and_output_gain() {
        let spectrum = Spectrum::default();
        assert_eq!(spectrum.bands(1.0), [0.0; 3]);
        for _ in 0..RING_SIZE + 32 {
            spectrum.push(0.1);
        }
        let playing = spectrum.bands(1.0);
        assert!(playing[0] > 0.9);
        assert!(spectrum.bands(0.1)[0] < playing[0]);
        assert_eq!(spectrum.bands(0.0), [0.0; 3]);
        for _ in 0..FFT_SIZE {
            spectrum.push(0.0);
        }
        assert_eq!(spectrum.bands(1.0), [0.0; 3]);
    }
}
