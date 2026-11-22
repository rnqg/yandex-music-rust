use crate::gui::{App, AppWindow};
use crate::player::{Cmd, Player};
use slint::{ComponentHandle, Timer, TimerMode};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use std::cell::RefCell;
use std::time::Duration;

struct Session {
    controls: MediaControls,
    track: Option<String>,
    duration: f32,
    last_playback: MediaPlayback,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.controls.set_playback(MediaPlayback::Stopped);
        let _ = self.controls.set_metadata(MediaMetadata::default());
    }
}

pub fn install(ui: &AppWindow, player: &Player) -> Option<Timer> {
    let mut controls = MediaControls::new(PlatformConfig {
        dbus_name: env!("CARGO_PKG_NAME"),
        display_name: env!("APP_NAME"),
        hwnd: None,
    })
    .map_err(|error| crate::config::log(format!("Пункт управления: {error}")))
    .ok()?;
    let weak = ui.as_weak();
    let remote_player = player.clone();
    controls
        .attach(move |event| {
            let player = remote_player.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let app = ui.global::<App>();
                let state = player.state();
                match event {
                    MediaControlEvent::Toggle => app.invoke_play_pause(),
                    MediaControlEvent::Play if !state.playing => app.invoke_play_pause(),
                    MediaControlEvent::Pause | MediaControlEvent::Stop if state.playing => {
                        app.invoke_play_pause()
                    }
                    MediaControlEvent::Next => player.send(Cmd::Next),
                    MediaControlEvent::Previous => player.send(Cmd::Prev),
                    MediaControlEvent::SetPosition(pos) => {
                        player.send(Cmd::Seek(pos.0.as_secs_f32()))
                    }
                    _ => {}
                }
            });
        })
        .map_err(|error| crate::config::log(format!("Медиаклавиши: {error}")))
        .ok()?;

    let session = RefCell::new(Session {
        controls,
        track: None,
        duration: 0.0,
        last_playback: MediaPlayback::Stopped,
    });
    let player = player.clone();
    let timer = Timer::default();
    timer.start(TimerMode::Repeated, Duration::from_millis(500), move || {
        let state = player.state();
        let mut session = session.borrow_mut();
        let track_id = state.track.as_ref().map(|track| track.id.as_str());
        let duration = state.track.as_ref().map_or(0.0, |track| {
            if state.duration > 0.0 {
                state.duration
            } else {
                track.duration_ms as f32 / 1000.0
            }
        });
        if session.track.as_deref() != track_id || session.duration != duration {
            if let Some(track) = &state.track {
                let cover = track.cover_url(400);
                let _ = session.controls.set_metadata(MediaMetadata {
                    title: Some(&track.title),
                    artist: Some(&track.artists),
                    album: Some(&track.album_title),
                    duration: Duration::try_from_secs_f32(duration).ok(),
                    cover_url: cover.as_deref(),
                });
            } else {
                let _ = session.controls.set_metadata(MediaMetadata::default());
            }
            session.track = track_id.map(str::to_owned);
            session.duration = duration;
        }
        let progress = Duration::try_from_secs_f32(state.position)
            .ok()
            .map(MediaPosition);
        let playback = match (track_id, state.playing) {
            (None, _) => MediaPlayback::Stopped,
            (Some(_), true) => MediaPlayback::Playing { progress },
            (Some(_), false) => MediaPlayback::Paused { progress },
        };
        if session.last_playback != playback {
            let _ = session.controls.set_playback(playback.clone());
            session.last_playback = playback;
        }
    });
    Some(timer)
}
