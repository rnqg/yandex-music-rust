use rodio::source::SeekError;
use rodio::Source;
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
struct Intro {
    pcm: Arc<Vec<f32>>,
    offset: usize,
}
use symphonia::core::{
    audio::SampleBuffer,
    codecs::Decoder as Codec,
    errors::{Error, Result},
    formats::{FormatReader, SeekMode, SeekTo},
    io::{MediaSource, MediaSourceStream},
    probe::Hint,
    units::TimeBase,
};

pub(crate) struct Decoder {
    format: Box<dyn FormatReader>,
    codec: Box<dyn Codec>,
    track_id: u32,
    time_base: Option<TimeBase>,
    duration: Option<Duration>,
    samples: Option<SampleBuffer<f32>>,
    offset: usize,
    rate: u32,
    channels: u16,
    ended: bool,
    intro: Option<Intro>,
}

impl Decoder {
    pub fn new(input: impl MediaSource + 'static) -> Result<Self> {
        let stream = MediaSourceStream::new(Box::new(input), Default::default());
        let format = symphonia::default::get_probe().format(&Hint::new(), stream, &Default::default(), &Default::default())?.format;
        let (track, codec) = format.tracks().iter().find_map(|track| {
            let codec = symphonia::default::get_codecs().make(&track.codec_params, &Default::default()).ok()?;
            Some((track, codec))
        }).ok_or(Error::Unsupported("no supported audio track"))?;
        let track_id = track.id;
        let time_base = track.codec_params.time_base;
        let duration = time_base.zip(track.codec_params.n_frames).map(|(base, n)| base.calc_time(n).into());
        let mut source = Self { format, codec, track_id, time_base, duration, samples: None,
            offset: 0, rate: 0, channels: 0, ended: false, intro: None };
        source.fill(None)?;
        Ok(source)
    }

    pub(crate) fn match_tempo(mut self, tempo: crate::crossfade::tempo::Tempo) -> Option<Self> {
        if self.channels > 2 || !(8_000..=96_000).contains(&self.rate) { return None; }
        let channels = usize::from(self.channels);
        let (frames, _) = tempo.frames(self.rate);
        let samples: Vec<_> = self.by_ref().take(frames.checked_mul(channels)?).collect();
        let intro = tempo.render(&samples, self.rate, channels)?;
        self.intro = Some(Intro { pcm: Arc::new(intro), offset: 0 });
        Some(self)
    }

    pub(crate) fn preview_intro(&mut self, prepared: &Self) {
        self.intro.clone_from(&prepared.intro);
    }

    fn fill(&mut self, target: Option<u64>) -> Result<()> {
        let mut errors = 0;
        loop {
            let packet = self.format.next_packet()?;
            if packet.track_id() != self.track_id { continue; }
            let pcm = match self.codec.decode(&packet) {
                Ok(pcm) => pcm,
                Err(Error::DecodeError(_)) if errors < 3 => { errors += 1; continue; }
                Err(err) => return Err(err),
            };
            let spec = *pcm.spec();
            let channels = spec.channels.count();
            if channels == 0 || spec.rate == 0 { return Err(Error::DecodeError("invalid audio format")); }
            let skip = target.zip(self.time_base).map_or(0, |(ts, base)| {
                let delta = base.calc_time(ts.saturating_sub(packet.ts()));
                ((delta.seconds as f64 + delta.frac) * f64::from(spec.rate)).round() as usize
            });
            if skip >= pcm.frames() { continue; }
            if self.samples.as_ref().is_none_or(|buf| buf.capacity() < pcm.frames() * channels) {
                self.samples = Some(SampleBuffer::new(pcm.capacity() as u64, spec));
            }
            self.samples.as_mut().unwrap().copy_interleaved_ref(pcm);
            self.rate = spec.rate;
            self.channels = channels as u16;
            self.offset = skip * channels;
            return Ok(());
        }
    }
}

impl Iterator for Decoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if let Some(intro) = &mut self.intro {
            if let Some(&sample) = intro.pcm.get(intro.offset) { intro.offset += 1; return Some(sample); }
            self.intro = None;
        }
        if self.offset == self.samples.as_ref()?.samples().len() {
            if self.ended { return None; }
            if self.fill(None).is_err() { self.ended = true; return None; }
        }
        let sample = self.samples.as_ref()?.samples()[self.offset];
        self.offset += 1;
        Some(sample)
    }
}

impl Source for Decoder {
    fn current_frame_len(&self) -> Option<usize> {
        if let Some(intro) = &self.intro { if intro.offset < intro.pcm.len() { return Some(intro.pcm.len() - intro.offset); } }
        self.samples.as_ref().map(|buf| buf.samples().len().saturating_sub(self.offset)).filter(|&n| n > 0)
    }
    fn channels(&self) -> u16 { self.channels }
    fn sample_rate(&self) -> u32 { self.rate }
    fn total_duration(&self) -> Option<Duration> { self.duration }

    fn try_seek(&mut self, position: Duration) -> std::result::Result<(), SeekError> {
        let channels = usize::from(self.channels);
        let channel = self.intro.as_ref().map_or(self.offset % channels, |intro| intro.offset % channels);
        let position = self.duration.map_or(position, |d| position.min(d.saturating_sub(Duration::from_millis(1))));
        let to = SeekTo::Time { time: position.into(), track_id: Some(self.track_id) };
        let map_err = |err| SeekError::Other(Box::new(err));
        let seek = self.format.seek(SeekMode::Accurate, to).map_err(map_err)?;
        self.intro = None;
        self.codec.reset();
        self.offset = self.samples.as_ref().map_or(0, |buf| buf.samples().len());
        self.ended = true;
        self.fill(Some(seek.required_ts)).map_err(map_err)?;
        self.ended = false;
        self.offset += channel.min(usize::from(self.channels) - 1);
        Ok(())
    }
}
