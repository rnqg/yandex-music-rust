use super::{harmony::Harmony, tempo::Tempo, Mix, Plan};

pub(crate) const EDGE_SECONDS: f32 = 15.0;

#[derive(Clone, Copy, Default)]
struct Level {
    rms: f32,
    bass: f32,
    mid: f32,
    high: f32,
}

pub(crate) struct Edge {
    bins: Vec<Level>,
    hop: f32,
    start: f32,
    eof: bool,
    harmony: Harmony,
}

#[derive(Clone, Copy)]
struct Pulse {
    period: f32,
    phase: f32,
}

impl Edge {
    pub(crate) fn read(mut pcm: impl Iterator<Item = f32>, rate: u32, channels: u16, start: f32) -> Option<Self> {
        if !(8_000..=192_000).contains(&rate) || !(1..=8).contains(&channels) { return None; }
        let frames = (rate + 50) / 100;
        let hop = frames as f32 / rate as f32;
        let window = if start == 0.0 { 32.0 } else { EDGE_SECONDS + 2.0 };
        let limit = (window / hop).ceil() as usize;
        let alpha = 1.0 - (-std::f32::consts::TAU * 160.0 / rate as f32).exp();
        let mut low = [0.0; 8];
        let upper_alpha = 1.0 - (-std::f32::consts::TAU * 2600.0 / rate as f32).exp();
        let mut upper = [0.0; 8];
        let stride = (rate / 5000).max(1);
        let mut pair = [0.0; 2];
        let mut subframes = 0;
        let mut tonal = Vec::with_capacity(limit * frames as usize / stride as usize);
        let mut bins = Vec::with_capacity(limit);
        let mut eof = false;
        'audio: for _ in 0..limit {
            let (mut sum, mut bass, mut mid, mut high) = (0.0, 0.0, 0.0, 0.0);
            for _ in 0..frames {
                for ch in 0..usize::from(channels) {
                    let Some(x) = pcm.next() else { eof = true; break 'audio };
                    low[ch] += alpha * (x - low[ch]);
                    upper[ch] += upper_alpha * (x - upper[ch]);
                    mid += (upper[ch] - low[ch]).powi(2);
                    high += (x - upper[ch]).powi(2);
                    if ch < 2 { pair[ch] += x; }
                    sum += x * x;
                    bass += low[ch] * low[ch];
                }
                subframes += 1;
                if subframes == stride {
                    tonal.push([pair[0] / stride as f32, pair[1] / stride as f32]);
                    pair = [0.0; 2];
                    subframes = 0;
                }
            }
            let count = (frames * u32::from(channels)) as f32;
            bins.push(Level { rms: (sum / count).sqrt(), bass: (bass / count).sqrt(), mid: (mid / count).sqrt(), high: (high / count).sqrt() });
        }
        if bins.len() < 12 { return None; }
        let harmony = Harmony::read(&tonal, rate as f32 / stride as f32);
        Some(Self { bins, hop, start, eof, harmony })
    }

    fn audible(&self) -> Option<(usize, usize)> {
        let peak = self.bins.iter().map(|v| v.rms).fold(0.0_f32, f32::max);
        if peak < 0.0004 { return None; }
        let gate = (peak * 0.008).clamp(0.00007, 0.001);
        let first = self.bins.iter().position(|v| v.rms > gate)?;
        let last = self.bins.iter().rposition(|v| v.rms > gate)?;
        Some((first, last + 1))
    }

    fn pulse(&self, first: usize, end: usize) -> Option<Pulse> {
        self.counted_pulse(first, end).or_else(|| self.pattern_pulse(first, end))
    }

    fn pattern_pulse(&self, first: usize, end: usize) -> Option<Pulse> {
        let section = &self.bins[first..end];
        if section.len() < 450 { return None; }
        let mut last = section[0];
        let mut novelty = Vec::with_capacity(section.len());
        let mut moments = [(0.0_f32, 0.0_f32); 3];
        let mut strongest = 0.0_f32;
        for &frame in section {
            let increase = [(frame.bass-last.bass).max(0.0), (frame.mid-last.mid).max(0.0), (frame.high-last.high).max(0.0)];
            let attack = increase.map(|x| (40.0 * x).ln_1p());
            for (i,&v) in attack.iter().enumerate() {
                moments[i].0 += v;
                moments[i].1 += v*v;
                strongest = strongest.max(increase[i]);
            }
            novelty.push(attack);
            last = frame;
        }
        if strongest < 0.006 { return None; }
        for m in &mut moments {
            m.0 /= novelty.len() as f32;
            m.1 = (m.1 / novelty.len() as f32 - m.0*m.0).max(0.0).sqrt().max(0.045);
        }
        for frame in &mut novelty {
            for (v, &(mean, dev)) in frame.iter_mut().zip(&moments) { *v = ((*v-mean) / dev).clamp(-2.0,5.0); }
        }
        let mut corr = [0.0_f32; 405];
        for (lag, score) in corr.iter_mut().enumerate().skip(15) {
            let (mut dot,mut left,mut right) = (0.0,0.0,0.0);
            for (x,y) in novelty[lag..].iter().zip(&novelty) {
                for ch in 0..3 {
                    dot += x[ch]*y[ch];
                    left += x[ch]*x[ch];
                    right += y[ch]*y[ch];
                }
            }
            *score = dot / (left * right).sqrt().max(0.001);
        }
        let at = |t: f32| {
            let i = t as usize;
            corr[i] + (corr[i+1] - corr[i])*t.fract()
        };
        let mut best = 0.0;
        let mut lag = 0.0;
        for step in 264..=800 {
            let l = step as f32 / 8.0;
            let (a,b,c) = (at(l),at(2.0*l),at(4.0*l));
            let votes = [a,b,c].iter().filter(|&&s| s > 0.13).count();
            if votes < 2 { continue; }
            let score = 0.32*a + 0.38*b + 0.30*c + 0.045*at(l*0.5)
                - 0.025*(l/52.0).log2().abs();
            if score > best { best = score; lag = l; }
        }
        if best < 0.19 { return None; }
        let mut phases = [0.0_f32; 32];
        for (i,v) in novelty.iter().enumerate() {
            let slot = ((i as f32 / lag).fract() * 32.0).round() as usize % 32;
            phases[slot] += v[0].max(0.0) + 0.8*v[1].max(0.0) + 0.35*v[2].max(0.0);
        }
        let strength = |i| phases[i] + 0.7*(phases[(i+1)%32] + phases[(i+31)%32]);
        let phase = (0..32).max_by(|&a,&b| strength(a).total_cmp(&strength(b)))?;
        if strength(phase) < phases.iter().sum::<f32>() * 0.10 { return None; }
        Some(Pulse { period: lag*self.hop, phase: self.start + (first as f32 + phase as f32 * lag / 32.0) * self.hop })
    }

    fn counted_pulse(&self, first: usize, end: usize) -> Option<Pulse> {
        let levels = &self.bins[first..end];
        if levels.len() < 250 { return None; }
        let mut previous = Level::default();
        let mut onsets = Vec::with_capacity(levels.len());
        for &v in levels {
            let rise = (v.rms - previous.rms).max(0.0) + 1.5 * (v.bass - previous.bass).max(0.0)
                + 0.65 * (v.high - previous.high).max(0.0);
            onsets.push(rise);
            previous = v;
        }
        let max = onsets.iter().copied().fold(0.0_f32, f32::max);
        if max < 0.004 { return None; }
        let mut peaks: Vec<(usize, f32)> = Vec::new();
        for (i, &v) in onsets.iter().enumerate().filter(|(_, v)| **v > max * 0.12) {
            if let Some(last) = peaks.last_mut().filter(|p| i - p.0 < 7) {
                if v > last.1 { *last = (i, v); }
            } else { peaks.push((i, v)); }
        }
        if peaks.len() < 5 { return None; }
        let mut best = 0.0;
        let mut grid = None;
        let weight: f32 = peaks.iter().map(|p| p.1).sum();
        for lag in 33..=85 {
            let mut real = 0.0;
            let mut imag = 0.0;
            for &(i, w) in &peaks {
                let angle = std::f32::consts::TAU * (i % lag) as f32 / lag as f32;
                let (sin, cos) = angle.sin_cos();
                real += w * cos;
                imag += w * sin;
            }
            let phase = imag.atan2(real) * lag as f32 / std::f32::consts::TAU;
            let mut aligned = 0;
            for &(i, _) in &peaks {
                let beat = (i as f32 - phase) / lag as f32;
                if (beat - beat.round()).abs() <= 0.12 { aligned += 1; }
            }
            let coverage = (aligned as f32 * lag as f32 / levels.len() as f32).min(1.0);
            let score = real.hypot(imag) / weight * coverage;
            if score > best {
                best = score;
                grid = Some((lag as f32, phase));
            }
        }
        if best < 0.52 {
            grid = subdivision_grid(&peaks, levels.len());
        }
        let (lag, phase) = grid?;
        let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (i, w) in peaks {
            let y = i as f32;
            let x = ((y - phase) / lag).round();
            if (y - phase - x * lag).abs() > 0.15 * lag { continue; }
            sw += w;
            sx += w * x;
            sy += w * y;
            sxx += w * x * x;
            sxy += w * x * y;
        }
        let variance = sw * sxx - sx * sx;
        if variance <= 0.0 { return None; }
        let slope = (sw * sxy - sx * sy) / variance;
        Some(Pulse { period: slope * self.hop, phase: self.start + (first as f32 + (sy - slope * sx) / sw) * self.hop })
    }

    fn bass_entry(&self, end: f32) -> Option<f32> {
        let opening = self.level(0.0, 2.0);
        let mut best = 0.08;
        let mut entry = None;
        let mut t = 2.0;
        while t < (end - 1.8).min(20.0) {
            let before = self.level(t - 1.0, t);
            let hit = self.level(t, t + 0.40);
            let after = self.level(t + 0.40, t + 1.6);
            let rise = hit.bass.min(after.bass) - before.bass;
            let score = rise + 0.25 * (after.rms - before.rms);
            if score > best && rise > before.bass * 0.60 && opening.bass < after.bass * 0.45
                && opening.rms < after.rms * 0.65 {
                best = score;
                entry = Some(t);
            }
            t += 0.025;
        }
        entry
    }

    fn entry_pulse(&self, first: usize, end: usize, at: f32) -> Option<Pulse> {
        let from = ((at / self.hop) as usize).clamp(first, end);
        self.pulse(from, (from + (13.0 / self.hop) as usize).min(end))
            .or_else(|| self.pulse(first,end))
            .or_else(|| self.pulse(first.max(end.saturating_sub((12.0 / self.hop) as usize)),end))
    }

    #[cfg(test)]
    pub(crate) fn measured_bpm(&self) -> Option<f32> {
        let (a, b) = self.audible()?;
        self.pulse(a,b).map(|p| 60.0 / p.period)
    }

    #[cfg(test)]
    pub(crate) fn levels(&self) -> Vec<(f32, f32, f32, f32)> {
        (0..(self.bins.len() as f32 * self.hop).ceil() as usize).map(|i| {
            let t = self.start + i as f32;
            let v = self.level(t, t+1.0);
            (v.rms, v.bass, v.mid, v.high)
        }).collect()
    }

    fn bar(&self, pulse: Pulse) -> Option<f32> {
        let mut strengths = [0.0; 4];
        let mut counts = [0u32; 4];
        let first = ((self.start - pulse.phase) / pulse.period).ceil() as i32;
        let end = self.start + self.hop * self.bins.len() as f32;
        for beat in first..=((end - pulse.phase - 0.07) / pulse.period).floor() as i32 {
            let time = pulse.phase + beat as f32 * pulse.period;
            let hit = self.level(time, time + 0.07);
            let before = self.level(time - 0.07, time);
            let slot = beat.rem_euclid(4) as usize;
            strengths[slot] += (hit.bass - before.bass).max(0.0) + 0.3 * (hit.rms - before.rms).max(0.0);
            counts[slot] += 1;
        }
        if counts.iter().any(|&n| n < 2) { return None; }
        for (s,n) in strengths.iter_mut().zip(counts) { *s /= n as f32; }
        let mut order = [0,1,2,3];
        order.sort_by(|&a,&b| strengths[b].total_cmp(&strengths[a]));
        (strengths[order[0]] > strengths[order[1]] * 1.23 + 0.005)
            .then_some(pulse.phase + order[0] as f32 * pulse.period)
    }

    fn level(&self, from: f32, to: f32) -> Level {
        let bin = |s: f32| (((s - self.start) / self.hop).round() as usize).min(self.bins.len());
        let slice = &self.bins[bin(from)..bin(to)];
        let count = slice.len().max(1) as f32;
        let power = |band: fn(&Level) -> f32| (slice.iter().map(|v| band(v).powi(2)).sum::<f32>() / count).sqrt();
        Level { rms: power(|v| v.rms), bass: power(|v| v.bass), mid: power(|v| v.mid), high: power(|v| v.high) }
    }
}

fn subdivision_grid(peaks: &[(usize, f32)], span: usize) -> Option<(f32, f32)> {
    let mut top = 0.0;
    let mut result = None;
    let total = peaks.iter().map(|&(_, w)| w.sqrt()).sum::<f32>();
    for eighths in 260..=740 {
        let period = eighths as f32 * 0.125;
        let mut histogram = [0.0; 24];
        for &(i, w) in peaks {
            let bin = ((i as f32 / period).fract() * 24.0).round() as usize % 24;
            histogram[bin] += w.sqrt();
        }
        let neighborhood = |i: usize| histogram[i] + histogram[(i+23)%24] * 0.8 + histogram[(i+1)%24] * 0.8;
        let (phase, primary, secondary) = (0..24).map(|i| (i,neighborhood(i),neighborhood((i+12)%24)))
            .max_by(|a,b| (a.1 + 0.55*a.2).total_cmp(&(b.1 + 0.55*b.2)))?;
        let score = (primary + 0.55 * secondary) / total;
        if score <= top || primary < total * 0.24 { continue; }
        let phase = phase as f32 * period / 24.0;
        let mut last = i32::MIN;
        let mut hits = 0;
        let mut halves = [0; 2];
        for &(i, _) in peaks {
            let beat = (i as f32 - phase) / period;
            let index = beat.round() as i32;
            if (beat - index as f32).abs() < 0.085 && index != last {
                last = index;
                hits += 1;
                halves[usize::from(i >= span / 2)] += 1;
            }
        }
        let expected = span as f32 / period;
        if hits < 5 || hits as f32 / expected < 0.40 || halves.iter().any(|&n| n < 2) { continue; }
        top = score;
        result = Some((period, phase));
    }
    if top >= 0.39 { result } else { None }
}

pub(crate) fn plan(out: &Edge, incoming: &Edge, duration: f32, next_duration: f32, seconds: u8) -> Option<Plan> {
    let (out_first, out_last) = out.audible()?;
    let (in_first, in_last) = incoming.audible()?;
    if !out.eof { return None; }
    let audible_end = out.start + out_last as f32 * out.hop;
    let file_end = out.start + out.bins.len() as f32 * out.hop;
    if (file_end - duration).abs() > 2.0 { return None; }
    let end = if file_end - audible_end > 0.35 {
        (audible_end + 0.12).max(file_end - 2.0)
    } else { file_end };
    let intro = in_first as f32 * incoming.hop;
    let next_duration = if incoming.eof { incoming.bins.len() as f32 * incoming.hop } else { next_duration };
    let limit = f32::from(seconds).min(end * 0.5).min(next_duration * 0.5);
    if limit < 0.1 { return None; }
    let mut length = limit;
    let mut alignment = None;
    let mut lock = None;
    let mut echo_ms = 0;
    let outgoing = out.pulse(out_first, out_last);
    let drop = incoming.bass_entry(in_last as f32 * incoming.hop);
    let next = incoming.entry_pulse(in_first, in_last, drop.unwrap_or(intro));
    let ratio = outgoing.zip(next).map(|(a,b)| {
        let r = b.period / a.period;
        if r > 1.5 { r * 0.5 } else if r < 0.7 { r * 2.0 } else { r }
    });
    if ratio.is_some_and(|r| (r - 1.0).abs() > 0.065) {
        length = length.min((limit * 0.45).clamp(0.75, 2.0));
    }
    let mut cue = 0.0;
    if let Some(hit) = drop.filter(|&at| at > length * 0.70 && length >= 1.0 && (outgoing.is_some() || next.is_some())) {
        let max_cue = (next_duration * 0.12).min(15.0);
        let desired = (hit - length * 0.57).clamp(0.0, max_cue);
        cue = if let Some(pulse) = next {
            let beat = pulse.phase + ((desired - pulse.phase) / pulse.period).round() * pulse.period;
            beat.clamp(0.0, (hit - 0.55).min(max_cue).max(0.0))
        } else { desired };
    }
    let intro = (intro - cue).max(0.0);
    if let (Some(a), Some(b), Some(ratio)) = (outgoing, next, ratio) {
        if (ratio - 1.0).abs() <= 0.065 {
            let speed = if (ratio - 1.0).abs() > 0.002 { ratio } else { 1.0 };
            alignment = Some((a.phase - (b.phase - cue) / speed, a.period));
            if (a.period - b.period).abs() < a.period * 0.07 {
                if let (Some(left), Some(right)) = (out.bar(a), incoming.bar(b)) {
                    alignment = Some((left - (right - cue) / speed, 4.0 * a.period));
                }
            }
            if speed != 1.0 { lock = Some(speed); }
        } else {
            alignment = Some((a.phase - (b.phase - cue), a.period));
            echo_ms = (500.0 * a.period).round() as u16;
        }
    }
    let loud = |v: Level| v.rms > 0.14;
    if drop.is_none() && alignment.is_none() && loud(out.level(end - length, end)) && loud(incoming.level(cue + intro, cue + intro + length)) {
        length = length.min((limit * 0.65).max(1.0));
    }
    length = length.max(intro + 0.5).min(limit);
    let mut start = end - length;
    if let Some((phase, beat)) = alignment {
        let aligned = phase + ((start - phase) / beat).ceil() * beat;
        if end - aligned >= (intro + 0.5).max(length * 0.5) { start = aligned; }
    }
    let mut cut_beat = None;
    if echo_ms > 0 {
        if let (Some(a), Some(b)) = (outgoing, next) {
            let target = intro + 0.29 * (end - start - intro).max(0.0);
            let entering = b.phase - cue + ((target + cue - b.phase) / b.period).round() * b.period;
            let leaving = a.phase + ((start + entering - a.phase) / a.period).round() * a.period;
            let proposed = leaving - entering;
            if proposed >= end - length && entering > intro + 0.06 && entering < (end - proposed) * 0.75 {
                start = proposed;
                cut_beat = Some(entering);
            }
        }
    }
    if let (Some(hit), Some(pulse)) = (drop, outgoing) {
        let elapsed = (hit - cue) / lock.unwrap_or(1.0);
        if elapsed > 0.15 && elapsed < length * 0.86 {
            let anchor = pulse.phase + ((end - length + elapsed - pulse.phase) / pulse.period).ceil() * pulse.period;
            let proposed = anchor - elapsed;
            if end - proposed >= elapsed + 0.16 {
                start = proposed;
                cut_beat = Some(elapsed);
            }
        }
    }
    let length = end - start;
    let left = out.level(start, end);
    let right = incoming.level(cue + intro, cue + (length * lock.unwrap_or(1.0)).max(intro));
    let has_bass = |v: Level| v.bass > 0.025 && v.bass > v.rms * 0.45;
    let bass = has_bass(left) && has_bass(right);
    let clash = out.harmony.clashes(start - out.start, end - out.start, &incoming.harmony, cue + intro, cue + length * lock.unwrap_or(1.0));
    let mids = left.mid.min(right.mid) > 0.045;
    let lead_in = (intro / lock.unwrap_or(1.0) - 0.08).clamp(0.0, 2.0).min((length - 0.1).max(0.0));
    let active = (length - lead_in).max(0.1);
    if echo_ms == 0 && next.is_none() && clash == Some(true) {
        if let Some(beat) = outgoing { echo_ms = (500.0 * beat.period).round() as u16; }
    }
    let pivot = if echo_ms > 0 { 0.29 } else { 0.5 };
    let mut swap = pivot;
    let mut width = 0.25;
    if let Some(beat) = outgoing {
        let bar = out.bar(beat);
        let origin = start + lead_in;
        let first = ((origin + active * 0.18 - beat.phase) / beat.period).ceil() as i32;
        let last = ((origin + active * 0.82 - beat.phase) / beat.period).floor() as i32;
        let mut high_score = f32::NEG_INFINITY;
        for n in first..=last {
            let hit = beat.phase + n as f32 * beat.period;
            let at = (hit - origin) / active;
            let entry = cue + (hit - start) * lock.unwrap_or(1.0);
            let window = beat.period * 0.45;
            let before = out.level(hit - window, hit).mid;
            let after = out.level(hit, hit + window).mid;
            let opens = incoming.level(entry, entry + window).mid - incoming.level(entry - window, entry).mid;
            let mut score = -1.8 * (at - pivot).abs()
                + 0.32 * ((before - after) / left.mid.max(0.025)).clamp(-1.0, 1.0)
                + 0.24 * (opens / right.mid.max(0.025)).clamp(-1.0, 1.0);
            if let Some(bar) = bar {
                let phase = (hit - bar) / (4.0 * beat.period);
                if (phase - phase.round()).abs() < 0.04 { score += 0.30; }
            }
            if score > high_score { swap = at; high_score = score; }
        }
        if let Some(hit) = cut_beat { swap = ((hit - lead_in) / active).clamp(0.15,0.85); }
        width = (beat.period * 0.55 / active).clamp(0.035, 0.32);
    }
    let tempo = lock.map(|ratio| Tempo { ratio, hold: length, release: (outgoing.unwrap().period * 4.0).clamp(1.2, 3.0) });
    Some(Plan { start, end, lead_in, cue, beat: outgoing.map(|p| p.period), tempo,
        mix: Mix { bass, dense: loud(left) && loud(right), bands: outgoing.is_some() && next.is_some() || mids && (clash == Some(true) || bass || left.high.min(right.high) > 0.018),
            tight_mid: echo_ms > 0 || mids && clash.unwrap_or(left.mid > left.rms * 0.68 && right.mid > right.rms * 0.68),
            hats: left.high.max(right.high) > 0.012, swap, width, echo_ms, trim: [0; 8] } })

}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording(rate: u32, seconds: f32, f: impl Fn(f32) -> f32) -> Edge {
        let samples = (0..(rate as f32 * seconds) as usize).flat_map(|i| {
            let x = f(i as f32 / rate as f32);
            [x, -x]
        });
        Edge::read(samples, rate, 2, 0.0).unwrap()
    }

    fn drums(rate: u32, bpm: f32, offset: f32) -> Edge {
        recording(rate, 14.0, |t| {
            if t < offset { return 0.0; }
            let beat = (t - offset) % (60.0 / bpm);
            0.8 * (-beat * 18.0).exp() * (beat * 65.0 * std::f32::consts::TAU).sin()
        })
    }

    #[test]
    fn matches_the_beat_phase_across_rates_and_stereo_polarity() {
        let out = drums(44_100, 127.0, 0.17);
        let incoming = drums(48_000, 126.0, 0.08);
        let mix = plan(&out, &incoming, 14.0, 14.0, 6).unwrap();
        let beats = (mix.start + 0.08 - 0.17) / (60.0 / 127.0);
        assert!((beats - beats.round()).abs() < 0.06, "start {}", mix.start);
        assert!((5.3..=6.01).contains(&(mix.end - mix.start)));
        assert!(mix.mix.bass);
    }

    #[test]
    fn clashing_melodies_use_a_narrower_midrange_swap_than_a_fifth() {
        let note = |hz: f32| recording(44_100, 12.0, |t| {
            0.22 * (std::f32::consts::TAU * hz * t).sin()
                + 0.27 * (-t.rem_euclid(0.5) * 28.0).exp() * (std::f32::consts::TAU * 64.0 * t).sin()
        });
        let a = note(261.63);
        let fifth = note(392.0);
        let semitone = note(277.18);
        assert_eq!(a.harmony.clashes(6.0, 12.0, &semitone.harmony, 0.0, 6.0), Some(true));
        assert_eq!(a.harmony.clashes(6.0, 12.0, &fifth.harmony, 0.0, 6.0), Some(false));
        let clash = plan(&a,&semitone,12.0,12.0,6).unwrap();
        let compatible = plan(&a,&fifth,12.0,12.0,6).unwrap();
        assert!(clash.mix.bands && clash.mix.tight_mid);
        assert!(!compatible.mix.tight_mid);
    }

    #[test]
    fn accents_align_across_four_beat_bars() {
        let bars = |accent: usize| recording(48_000, 15.0, |t| {
            let beat = (t * 2.0).floor() as usize;
            let amp = if beat % 4 == accent { 0.9 } else { 0.28 };
            let phase = t % 0.5;
            amp * (-phase * 18.0).exp() * (phase * 64.0 * std::f32::consts::TAU).sin()
        });
        let a = bars(0);
        let b = bars(1);
        let mix = plan(&a, &b, 15.0, 15.0, 6).unwrap();
        let bars = (mix.start + 0.5) / 2.0;
        assert!((bars - bars.round()).abs() < 0.026, "start {}", mix.start);
        assert!(mix.mix.bands && mix.mix.bass);
    }

    #[test]
    fn the_first_sustained_bass_entry_lands_on_an_outgoing_beat() {
        let out = drums(44_100, 120.0, 0.12);
        let incoming = recording(44_100, 32.0, |t| {
            let base = 0.035 * (t * std::f32::consts::TAU * 330.0).sin();
            if t < 9.0 { return base; }
            let beat = (t - 9.0).rem_euclid(0.5);
            base + 0.55 * (t * std::f32::consts::TAU * 64.0).sin() * (0.75 + 0.25 * (-beat * 15.0).exp())
        });
        let mix = plan(&out, &incoming,14.0,120.0,6).unwrap();
        let cue = mix.cue;
        let at = mix.start + (9.0 - cue);
        let beats = (at - 0.12) * 2.0;
        assert!(cue > 1.0 && cue < 9.0);
        assert!((beats - beats.round()).abs() < 0.10, "cue {cue} at {at}");
        assert!((mix.start + (mix.end - mix.start) * mix.mix.swap - at).abs() < 0.10);
    }

    #[test]
    fn incompatible_tempos_get_a_shorter_overlap() {
        let out = drums(44_100, 120.0, 0.1);
        let incoming = drums(48_000, 94.0, 0.35);
        let mix = plan(&out, &incoming, 14.0, 14.0, 9).unwrap();
        assert!(mix.end - mix.start <= 2.01);
        assert!((230..270).contains(&mix.mix.echo_ms));
    }

    #[test]
    fn silence_noise_and_dense_music_use_different_transitions() {
        let tone = |t: f32| 0.6 * (t * 100.0 * std::f32::consts::TAU).sin();
        let out = recording(48_000, 14.0, |t| if t < 13.0 { tone(t) } else { 0.0 });
        let incoming = recording(44_100, 14.0, |t| if t > 0.8 { tone(t) } else { 0.0 });
        let mix = plan(&out, &incoming, 14.0, 14.0, 6).unwrap();
        assert!((13.1..13.15).contains(&mix.end));
        assert!((0.70..=0.74).contains(&mix.lead_in));
        assert!(mix.mix.dense && mix.mix.bass);
        assert!(mix.end - mix.start < 4.0);
        let quiet = recording(44_100, 14.0, |t| 0.03 * tone(t));
        let gentle = plan(&quiet, &quiet, 14.0, 14.0, 6).unwrap();
        assert!(!gentle.mix.dense && !gentle.mix.bass);
        assert!((gentle.end - gentle.start - 6.0).abs() < 0.01);
        let silent = recording(44_100, 4.0, |_| 0.0);
        assert!(plan(&silent, &incoming, 4.0, 14.0, 6).is_none());
    }
}
