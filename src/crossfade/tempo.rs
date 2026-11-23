#[derive(Clone, Copy, Debug)]
pub(crate) struct Tempo {
    pub ratio: f32,
    pub hold: f32,
    pub release: f32,
}

impl Tempo {
    pub fn media_time(self, output: f32) -> f32 {
        let ramp = (output - self.hold).clamp(0.0, self.release);
        output + (self.ratio - 1.0) * (output.min(self.hold) + ramp * (1.0 - 0.5 * ramp / self.release))
    }

    pub fn frames(self, rate: u32) -> (usize, usize) {
        let output = ((self.hold + self.release + 0.09) * rate as f32).ceil() as usize;
        let input = (self.media_time(output as f32 / rate as f32) * rate as f32).round() as usize;
        (input, output)
    }

    pub fn render(self, pcm: &[f32], rate: u32, channels: usize) -> Option<Vec<f32>> {
        let (input, output) = self.frames(rate);
        if channels == 0 || channels > 2 || pcm.len() != input * channels { return None; }
        let hop = (rate as usize * 20 / 1000).max(1);
        let search = rate as usize * 6 / 1000;
        if input.min(output) < hop * 5 { return None; }
        let mut audio = vec![0.0; output * channels];
        audio[..2 * hop * channels].copy_from_slice(&pcm[..2 * hop * channels]);
        let window: Vec<_> = (0..hop).map(|i| {
            let x = i as f32 / hop as f32;
            x * x * (3.0 - 2.0 * x)
        }).collect();
        let stride = (rate as usize / 6000).max(1);
        let mut next = hop;
        while next < output {
            let count = (output - next).min(2 * hop);
            let tail = output - next <= 4 * hop;
            let center = if tail { input - (output - next) } else {
                (self.media_time(next as f32 / rate as f32) * rate as f32).round() as usize
            }.min(input - count);
            let mut source = center;
            if !tail {
                let min = center.saturating_sub(search);
                let max = (center + search).min(input - count);
                let score = |at: usize| {
                    let (mut dot, mut energy) = (0.0, 0.000000001);
                    for i in (0..hop).step_by(stride) {
                        for ch in 0..channels {
                            let a = pcm[(at + i) * channels + ch];
                            dot += a * audio[(next + i) * channels + ch];
                            energy += a * a;
                        }
                    }
                    dot / energy.sqrt() - 0.00001 * at.abs_diff(center) as f32 / search.max(1) as f32
                };
                let mut best = score(center);
                for at in (min..=max).step_by(stride) {
                    let s = score(at);
                    if s > best { source = at; best = s; }
                }
                for at in source.saturating_sub(stride).max(min)..=(source + stride).min(max) {
                    let s = score(at);
                    if s > best { source = at; best = s; }
                }
            }
            for i in 0..count.min(hop) {
                for ch in 0..channels {
                    let old = &mut audio[(next + i) * channels + ch];
                    *old += window[i] * (pcm[(source + i) * channels + ch] - *old);
                }
            }
            if count > hop {
                audio[(next + hop) * channels..(next + count) * channels]
                    .copy_from_slice(&pcm[(source + hop) * channels..(source + count) * channels]);
            }
            next += hop;
        }
        Some(audio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_repeated_drums_preserves_pitch_stereo_and_rejoins_the_recording() {
        let rate = 44_100;
        for native_bpm in [116.0, 125.0] {
            let tempo = Tempo { ratio: 120.0 / native_bpm, hold: 6.0, release: 2.0 };
            let (input, output) = tempo.frames(rate);
            let mut native = Vec::with_capacity(input * 2);
            for i in 0..input {
                let t = i as f32 / rate as f32;
                let phase = (t * native_bpm / 60.0).fract() * 60.0 / native_bpm;
                let kick = 0.75 * (-phase * 50.0).exp() * (std::f32::consts::TAU * 73.0 * phase).sin();
                let tone = 0.18 * (std::f32::consts::TAU * 440.0 * t).sin();
                native.extend([kick + tone, kick - tone]);
            }
            let mix = tempo.render(&native, rate, 2).unwrap();
            assert_eq!(mix.len(), output * 2);
            let chord = |i: usize| (mix[2*i] - mix[2*i+1]) * 0.5;
            let cycles = (rate as usize..rate as usize * 5).filter(|&i| chord(i) < 0.0 && chord(i+1) >= 0.0).count();
            assert!((cycles as f32 / 4.0 - 440.0).abs() < 2.0, "cycles {cycles}");
            for beat in 2..12 {
                let target = (rate as usize * beat) / 2;
                let radius = rate as usize / 12;
                let (actual, _) = (target-radius..target+radius).step_by(24).map(|i| {
                    let energy: f32 = (i..i+220).map(|n| (mix[2*n] + mix[2*n+1]).powi(2)).sum();
                    (i,energy)
                }).max_by(|a,b| a.1.total_cmp(&b.1)).unwrap();
                assert!(actual.abs_diff(target) < rate as usize / 32, "beat {beat}, error {} frames", actual.abs_diff(target));
            }
            assert_eq!(&mix[mix.len()-800..], &native[native.len()-800..]);
            assert!(mix.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
            assert!((tempo.media_time(10.0) - tempo.media_time(9.0) - 1.0).abs() < 0.00001);
        }
    }
}
