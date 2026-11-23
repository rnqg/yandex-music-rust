pub(crate) mod analysis;
mod harmony;
pub(crate) mod tempo;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub type Control = Arc<FadeControl>;

#[derive(Clone, Copy, Default, Debug)]
pub struct Mix {
    pub bass: bool,
    pub dense: bool,
    pub bands: bool,
    pub tight_mid: bool,
    pub hats: bool,
    pub echo_ms: u16,
    pub swap: f32,
    pub width: f32,
    pub trim: [u8; 8],
}

#[derive(Clone, Copy)]
pub(crate) struct Plan {
    pub start: f32,
    pub end: f32,
    pub lead_in: f32,
    pub cue: f32,
    pub beat: Option<f32>,
    pub mix: Mix,
    pub tempo: Option<tempo::Tempo>,
}

impl Plan {
    pub fn basic(seconds: u8, duration: f32, next_duration: f32) -> Self {
        let length = f32::from(seconds).min(duration * 0.5).min(next_duration * 0.5);
        Self { start: duration - length, end: duration, lead_in: 0.0, cue: 0.0, beat: None, tempo: None, mix: Mix::default() }
    }

    pub fn catch_up(&mut self, position: f32) {
        let late = position - self.start;
        if late <= 0.075 || self.end - position < 0.14 { return; }
        let next = self.beat.map(|beat| {
            self.start + ((late - 0.075) / beat).ceil() * beat
        }).unwrap_or(position);
        let revised = if self.end - next > 0.45 { next } else { position };
        let previous = self.end - self.start - self.lead_in;
        let active = (self.end - revised - self.lead_in).max(0.1);
        self.mix.swap = (self.mix.swap * previous / active).clamp(0.15, 0.85);
        self.mix.width = (self.mix.width * previous / active).clamp(0.035, 0.32);
        self.start = revised;
    }

    pub fn fade_ms(&self, position: f32) -> Option<i32> {
        let remaining = self.end - position;
        (position >= self.start && remaining >= 0.10).then(|| (remaining * 1000.0).round() as i32)
    }
}

#[derive(Default)]
pub struct FadeControl {
    command: AtomicU64,
    trim: AtomicU64,
}

impl FadeControl {
    pub fn start(&self, ms: i32, lead_ms: u16, mix: Mix) {
        let quantize = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u64;
        let word = u64::from(ms.unsigned_abs().clamp(1, 65_535)) | (u64::from(ms < 0) << 16)
            | (u64::from(mix.bass) << 17) | (u64::from(mix.dense) << 18) | (u64::from(lead_ms) << 32)
            | (u64::from(mix.bands) << 19) | (u64::from(mix.tight_mid) << 20) | (u64::from(mix.hats) << 21) | (u64::from(mix.echo_ms.min(511)) << 22)
            | (quantize(mix.swap) << 48) | (quantize(mix.width) << 56);
        self.trim.store(u64::from_le_bytes(mix.trim), Ordering::Relaxed);
        self.command.store(word, Ordering::Release);
    }

    pub fn reset(&self) { self.command.store(0, Ordering::Release); }
}

pub struct Envelope {
    pub echo_ms: u16,
    pub echo_send: f32,
    pub echo_wet: f32,
    pub headroom: f32,
    trim_pos: f32,
    trim_step: f32,
    trim_edge: f32,
    guard: bool,
    control: Control,
    command: u64,
    wait_frames: u32,
    frames_to_poll: u8,
    rate: f64,
    phase: f64,
    step: f64,
    gain: f32,
    mix: Mix,
    outgoing: bool,
    pub low_delta: f32,
    pub mid_delta: f32,
}

impl Envelope {
    pub fn new(control: Control, rate: u32) -> Self {
        let word = control.command.load(Ordering::Acquire);
        let incoming = word != 0 && word & (1 << 16) == 0;
        let phase = if incoming { 0.0 } else { 1.0 };
        Self { control, command: u64::MAX, wait_frames: 0, frames_to_poll: 0, rate: rate.max(1) as f64, phase,
            step: 0.0, gain: phase as f32, mix: Mix::default(), outgoing: false, low_delta: 0.0, mid_delta: 0.0, echo_ms: 0, echo_send: 0.0, echo_wet: 0.0, headroom: 1.0, trim_pos: 0.0, trim_step: 0.0, trim_edge: 0.0, guard: false }
    }

    #[inline]
    pub fn frame(&mut self) -> f32 {
        if self.frames_to_poll == 0 {
            let command = self.control.command.load(Ordering::Acquire);
            if command != self.command {
                self.command = command;
                if command != 0 {
                    self.outgoing = command & (1 << 16) != 0;
                    self.mix = Mix { bass: command & (1 << 17) != 0, dense: command & (1 << 18) != 0,
                        bands: command & (1 << 19) != 0, tight_mid: command & (1 << 20) != 0, hats: command & (1 << 21) != 0,
                        echo_ms: ((command >> 22) & 511) as u16, swap: ((command >> 48) & 255) as f32 / 255.0, width: (command >> 56) as f32 / 255.0,
                        trim: self.control.trim.load(Ordering::Relaxed).to_le_bytes() };
                    self.low_delta = 0.0;
                    self.mid_delta = 0.0;
                }
                self.echo_ms = if command != 0 && self.outgoing { self.mix.echo_ms } else { 0 };
                self.echo_wet = 0.0;
                self.echo_send = 0.0;
                let ms = if command == 0 { 8 } else { command & 0xffff };
                self.wait_frames = (((command >> 32) & 0xffff) as f64 * self.rate / 1000.0).round() as u32;
                if command != 0 {
                    let total = ms as f64 * self.rate / 1000.0 + f64::from(self.wait_frames);
                    self.trim_pos = 0.0;
                    self.trim_step = (1.0 / total.max(1.0)) as f32;
                    self.trim_edge = (total / (0.015 * self.rate)).max(2.0) as f32;
                    self.guard = self.mix.trim != [0; 8];
                    self.headroom = 1.0;
                } else { self.guard = false; }
                let direction = if command != 0 && self.outgoing { -1.0 } else { 1.0 };
                self.step = direction * 1000.0 / (ms as f64 * self.rate);
            }
        }
        self.frames_to_poll = self.frames_to_poll.wrapping_sub(1);
        if self.guard {
            self.trim_pos = (self.trim_pos + self.trim_step).min(1.0);
            let bin = (self.trim_pos * 8.0 - 0.5).clamp(0.0, 7.0);
            let i = bin as usize;
            let left = f32::from(self.mix.trim[i]);
            let right = f32::from(self.mix.trim[(i+1).min(7)]);
            let edge = (self.trim_pos.min(1.0 - self.trim_pos) * self.trim_edge).min(1.0);
            self.headroom = 1.0 - (left + (right-left) * bin.fract()) * edge / 255.0;
            if self.trim_pos >= 1.0 { self.guard = false; }
        } else if self.headroom < 1.0 {
            self.headroom = (self.headroom + (1.0 / (0.012 * self.rate)) as f32).min(1.0);
        }
        let gain = self.gain;
        if self.step != 0.0 {
            if self.wait_frames != 0 {
                self.wait_frames -= 1;
                return self.gain;
            }
            self.phase = (self.phase + self.step).clamp(0.0, 1.0);
            self.gain = if self.mix.dense { self.phase as f32 } else { (self.phase * std::f64::consts::FRAC_PI_2).sin() as f32 };
            if self.mix.bands {
                let t = if self.outgoing { 1.0 - self.phase as f32 } else { self.phase as f32 };
                let smooth = |a: f32, b: f32| {
                    let u = ((t - a) / (b - a)).clamp(0.0, 1.0);
                    u * u * (3.0 - 2.0 * u)
                };
                let gain = |p: f32| {
                    let x = if self.outgoing { 1.0 - p } else { p };
                    if self.mix.dense { x } else { x.sqrt() }
                };
                let w = self.mix.width.max(0.03);
                let swap = self.mix.swap.clamp(0.15, 0.85);
                let mid_window = if self.mix.tight_mid { w.max(0.07) } else { 0.40 };
                let mid = gain(smooth((swap - mid_window).max(0.0), (swap + mid_window).min(1.0)));
                let high = if self.mix.hats { gain(smooth(0.0, 0.76)) } else { mid };
                let low = if self.mix.bass { gain(smooth((swap-w).max(0.0), (swap+w).min(1.0))) } else { mid };
                if self.echo_ms > 0 {
                    self.echo_send = 1.0 - smooth(swap, (swap + 0.12).min(1.0));
                    self.echo_wet = 0.32 * smooth(0.0, swap) * (1.0 - smooth(swap, 1.0));
                    if t >= 1.0 { self.echo_ms = 0; }
                }
                let headroom = 1.0 - 0.12 * (4.0 * t * (1.0 - t));
                self.gain = high * headroom;
                self.low_delta = (low - mid) * headroom;
                self.mid_delta = (mid - high) * headroom;
            } else if self.mix.bass {
                let threshold = if self.outgoing { 0.36 } else { 0.20 };
                let x = ((self.phase as f32 - threshold) / 0.54).clamp(0.0, 1.0);
                self.low_delta = -self.gain * 0.85 * (1.0 - x * x * (3.0 - 2.0 * x));
            }
            if self.phase == 0.0 || self.phase == 1.0 { self.step = 0.0; }
        }
        gain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_power_transition_reaches_both_endpoints_at_the_sample_rate() {
        for rate in [44_100, 48_000] {
            let envelope = |ms| {
                let control = Control::default();
                control.start(ms, 0, Mix::default());
                Envelope::new(control, rate)
            };
            let mut left = envelope(-1000);
            let mut right = envelope(1000);
            for frame in 0..=rate {
                let (a, b) = (left.frame(), right.frame());
                assert!((a * a + b * b - 1.0).abs() < 0.00001);
                if frame == 0 { assert_eq!((a,b), (1.0,0.0)); }
                if frame == rate / 2 { assert!((a - b).abs() < 0.0001); }
                if frame == rate { assert!(a < 0.00001 && b > 0.99999); }
            }
        }
    }

    #[test]
    fn silent_lead_in_holds_the_outgoing_level_and_the_bass_handoff_recovers() {
        let left = Control::default();
        let right = Control::default();
        let mix = Mix { bass: true, dense: true, ..Mix::default() };
        left.start(-1000, 300, mix);
        right.start(1000, 300, mix);
        let mut out = Envelope::new(left.clone(), 48_000);
        let mut incoming = Envelope::new(right, 48_000);
        for _ in 0..14_400 {
            assert_eq!((out.frame(), incoming.frame()), (1.0, 0.0));
        }
        for _ in 0..24_000 {
            assert!((out.frame() + incoming.frame() - 1.0).abs() < 0.0001);
        }
        assert!(out.low_delta < incoming.low_delta && incoming.low_delta < -0.1);
        left.reset();
        for _ in 0..1000 { out.frame(); }
        assert_eq!((out.frame(), out.low_delta), (1.0, 0.0));
        for _ in 0..25_000 { incoming.frame(); }
        assert_eq!((incoming.frame(), incoming.low_delta), (1.0, 0.0));
    }

    #[test]
    fn late_analysis_can_enter_on_the_following_beat_without_losing_the_cue() {
        let mut plan = Plan::basic(6, 30.0, 50.0);
        plan.beat = Some(0.5);
        plan.cue = 9.25;
        plan.catch_up(24.15);
        assert_eq!(plan.start, 24.5);
        plan.catch_up(24.515);
        assert_eq!(plan.start, 24.5);
        assert_eq!(plan.cue, 9.25);
    }

    #[test]
    fn transition_is_opt_in_and_short_tracks_keep_a_solo_section() {
        assert_eq!(Plan::basic(0, 80.0, 60.0).fade_ms(77.0), None);
        assert_eq!(Plan::basic(6, 80.0, 6.0).fade_ms(76.0), None);
        assert_eq!(Plan::basic(6, 80.0, 6.0).fade_ms(77.1), Some(2900));
    }
}
