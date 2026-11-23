use crate::api::Track;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackRules {
    pub blocked: BTreeMap<String, String>,
    pub block_features: bool,
    pub skip_speed: bool,
    pub skip_remix: bool,
    pub skip_live: bool,
    pub unique_wave: bool,
    pub stop_at_end: bool,
}

impl Default for PlaybackRules {
    fn default() -> Self {
        Self {
            blocked: BTreeMap::new(),
            block_features: true,
            skip_speed: true,
            skip_remix: true,
            skip_live: true,
            unique_wave: true,
            stop_at_end: true,
        }
    }
}

impl PlaybackRules {
    pub fn blocked_artist(&self, track: &Track) -> bool {
        track.artist_list.iter().take(if self.block_features { usize::MAX } else { 1 })
            .any(|artist| self.blocked.contains_key(&artist.id))
    }

    pub fn allows(&self, track: &Track, wave: bool) -> bool {
        if track.user_requested {
            return true;
        }
        if self.blocked_artist(track) {
            return false;
        }
        if !wave {
            return true;
        }
        let version_blocked = |label: &str| {
            let lower = label.to_lowercase();
            let words: Vec<_> = lower.split(|c: char| !c.is_alphanumeric()).filter(|s| !s.is_empty()).collect();
            let has = |options: &[&str]| words.iter().any(|word| options.contains(word));
            (self.skip_remix && has(&["remix", "ремикс"]))
                || (self.skip_live && has(&["live", "лайв", "концертная", "концертный"]))
                || (self.skip_speed && (has(&["slowed", "spedup", "speedup", "nightcore", "ускоренная", "замедленная"])
                    || words.windows(2).any(|w| matches!(w, ["sped" | "speed", "up"]))))
        };
        !version_blocked(&track.version)
            && !track.title.split(['(', '[']).skip(1).any(|s| version_blocked(s.split([')', ']']).next().unwrap_or(s)))
            && ![" - ", " — ", " – "].iter().any(|sep| track.title.rsplit_once(sep).is_some_and(|(_, suffix)| version_blocked(suffix)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_versions_and_credited_artists_except_manual_selections() {
        let mut rules = PlaybackRules::default();
        let track = |title: &str, version: &str| crate::api::parse_track(&json!({
            "id":1,"title":title,"version":version,"artists":[{"id":8,"name":"А"},{"id":9,"name":"Б"}]
        })).unwrap();
        let original = track("Live Forever", "");
        assert!(rules.allows(&original, true));
        for version in ["Sped Up", "Slowed + Reverb", "Club Remix", "Live at Wembley"] {
            assert!(!rules.allows(&track("Song", version), true));
        }
        assert!(!rules.allows(&track("Song (DJ Remix)", ""), true));
        rules.skip_live = false;
        assert!(rules.allows(&track("Song — Live", ""), true));
        rules.blocked.insert("9".into(), "Б".into());
        assert!(!rules.allows(&original, false));
        rules.block_features = false;
        assert!(rules.allows(&original, false));
        rules.blocked.insert("8".into(), "А".into());
        let mut requested = original;
        requested.user_requested = true;
        assert!(rules.allows(&requested, true));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn daily_window_survives_restart_and_is_scoped_to_account() {
        let dir = tempfile::tempdir().unwrap();
        let start = 90000;
        WaveHistory::load(dir.path(), "alice", start).record("42", start).unwrap();
        let restored = WaveHistory::load(dir.path(), "alice", start + 20);
        assert!(restored.contains("42", start + DAY - 1));
        assert!(!restored.contains("42", start + DAY));
        assert!(!WaveHistory::load(dir.path(), "bob", start + 20).contains("42", start + 20));
    }
}

const DAY: u64 = 24 * 60 * 60;

#[derive(Default)]
pub struct WaveHistory {
    times: BTreeMap<String, u64>,
    path: PathBuf,
}

impl WaveHistory {
    pub fn load(dir: &Path, uid: &str, now: u64) -> Self {
        let path = dir.join(format!("wave-history-{:x}.json", Sha256::digest(uid.as_bytes())));
        let times = std::fs::read(&path).ok().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default();
        let mut history = Self { times, path };
        history.prune(now);
        history
    }

    pub fn contains(&self, id: &str, now: u64) -> bool {
        self.times.get(id).is_some_and(|&time| now.saturating_sub(time) < DAY)
    }

    fn prune(&mut self, now: u64) {
        self.times.retain(|_, time| now.saturating_sub(*time) < DAY);
    }

    pub fn record(&mut self, id: &str, now: u64) -> io::Result<()> {
        self.prune(now);
        self.times.insert(id.to_owned(), now);
        if self.path.as_os_str().is_empty() { return Ok(()); }
        if let Some(parent) = self.path.parent() { std::fs::create_dir_all(parent)?; }
        let tmp = self.path.with_extension(format!("{}.new", std::process::id()));
        let result = (|| {
            std::fs::write(&tmp, serde_json::to_vec(&self.times)?)?;
            std::fs::rename(&tmp, &self.path)
        })();
        if result.is_err() { let _ = std::fs::remove_file(tmp); }
        result
    }
}
