//! Настройки (config.json), папка данных приложения и лог.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// Токен OAuth в памяти. На диске лежит только зашифрованным (`token_sealed`).
    #[serde(skip)]
    pub token: Option<String>,
    #[serde(skip)]
    pub refresh_token: Option<String>,
    /// Токены под DPAPI или в связке ключей Linux (src/secret.rs).
    #[cfg(not(target_os = "macos"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    token_sealed: Option<String>,
    #[cfg(not(target_os = "macos"))]
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_sealed: Option<String>,
    /// Токен открытым текстом: старый формат или Linux без связки ключей. Как только спрятать есть
    /// куда, при запуске переносится туда.
    #[cfg(not(target_os = "macos"))]
    #[serde(rename = "token", skip_serializing_if = "Option::is_none")]
    token_plain: Option<String>,
    /// Unix-время, когда токен истечёт.
    pub token_expires: Option<u64>,
    pub volume: f32,
    pub discord_enabled: bool,
    pub discord_app_id: String,
    /// system / light / dark
    pub theme: String,
    /// 0 превосходное, 1 оптимальное, 2 экономичное
    pub quality: u8,
    pub crossfade: bool,
    pub crossfade_seconds: u8,
    pub eq_on: bool,
    pub eq_preset: usize,
    pub eq_preamp: f32,
    pub eq_gains: [f32; 10],
    pub eq_frequencies: [f32; 10],
    pub eq_q: [f32; 10],
    /// Язык интерфейса Яндекса: ru, en, uz, kk — уходит в Accept-Language.
    pub lang: String,
    pub navbar_collapsed: bool,
    /// smooth — все анимации, lite — без анимаций и с урезанными кэшами.
    pub mode: String,
    /// Куда скачивать треки. Пусто — Музыка\Яндекс Музыка.
    pub download_dir: String,
    /// Скачивать FLAC, если он есть у трека.
    pub download_flac: bool,
    /// Блоки главной и «Книг и подкастов», которые скрыл пользователь.
    pub hidden_blocks: Vec<HiddenBlock>,
    /// Раз в полчаса спрашивать GitHub о новой версии.
    pub check_updates: bool,
    /// «Треки 18+» в меню профиля.
    pub allow_explicit: bool,
    pub playback_rules: crate::playback_rules::PlaybackRules,
    /// Обычное окно при выходе, в пикселях экрана. На Wayland места окна не узнать.
    pub window_pos: Option<[i32; 2]>,
    pub window_size: Option<[u32; 2]>,
    pub window_maximized: bool,
    /// Место мини-плеера при выходе.
    pub mini_pos: Option<[i32; 2]>,
}

/// Блок запоминается по id из скелета: он постоянный («recommended_playlists»), а заголовок
/// меняется («Летняя» → «Осенняя»). Заголовок — только чтобы показать список в настройках.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct HiddenBlock {
    pub id: String,
    pub title: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            token: None,
            refresh_token: None,
            #[cfg(not(target_os = "macos"))]
            token_sealed: None,
            #[cfg(not(target_os = "macos"))]
            refresh_sealed: None,
            #[cfg(not(target_os = "macos"))]
            token_plain: None,
            token_expires: None,
            volume: 0.6,
            discord_enabled: true,
            discord_app_id: String::new(),
            theme: "system".into(),
            quality: 0,
            crossfade: false,
            crossfade_seconds: 6,
            eq_on: false,
            eq_preset: 1,
            eq_preamp: 1.0,
            eq_gains: [0.0; 10],
            eq_frequencies: crate::eq::FREQS,
            eq_q: [crate::eq::DEFAULT_Q; 10],
            lang: "ru".into(),
            navbar_collapsed: false,
            mode: "smooth".into(),
            download_dir: String::new(),
            download_flac: false,
            hidden_blocks: Vec::new(),
            check_updates: true,
            allow_explicit: true,
            playback_rules: Default::default(),
            window_pos: None,
            window_size: None,
            window_maximized: false,
            mini_pos: None,
        }
    }
}

pub enum LoginStorage {
    Current,
    #[cfg(not(target_os = "macos"))]
    NeedsSealing,
    #[cfg(target_os = "macos")]
    Unavailable,
}

/// %APPDATA%\yamusic-rs или ~/.config/yamusic-rs. Папка прежнего названия (yamusic-lite, ещё раньше yawave) переносится
/// молча, вместе с токеном и настройками. Если её держит запущенная старая версия, этот запуск
/// работает со старой папкой, а перенос повторится в следующий раз.
pub fn dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let base = crate::sys::config_base();
        let dir = base.join("yamusic-rs");
        if !dir.exists() {
            if let Some(old) = ["yamusic-lite", "yawave"].iter().map(|n| base.join(n)).find(|p| p.exists()) {
                if std::fs::rename(&old, &dir).is_err() {
                    return old;
                }
            }
        }
        dir
    })
    .clone()
}

/// Скачанные шрифты и значки. На Windows лежат рядом с настройками, на Linux — в ~/.cache.
pub fn cache_dir() -> PathBuf {
    #[cfg(windows)]
    return dir();
    #[cfg(not(windows))]
    return crate::sys::cache_base().join("yamusic-rs");
}

impl Config {
    pub fn load_settings() -> Self {
        std::fs::read_to_string(dir().join("config.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    #[cfg(target_os = "macos")]
    pub fn has_saved_login(&self) -> bool {
        crate::session_file::exists(&crate::session_file::directory())
    }

    #[cfg(not(target_os = "macos"))]
    pub fn has_saved_login(&self) -> bool {
        self.token_sealed.is_some() || self.refresh_sealed.is_some()
            || self.token_plain.as_deref().is_some_and(|token| !token.trim().is_empty())
    }

    #[cfg(target_os = "macos")]
    pub fn resolve_login(&mut self) -> LoginStorage {
        if self.token.is_some() || self.refresh_token.is_some() {
            return LoginStorage::Current;
        }
        match crate::session_file::load(&crate::session_file::directory()) {
            Ok(saved) => {
                self.token = saved.token;
                self.refresh_token = saved.refresh;
                self.token_expires = saved.expires;
                LoginStorage::Current
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData) => LoginStorage::Current,
            Err(_) => LoginStorage::Unavailable,
        }
    }

    #[cfg(not(target_os = "macos"))]
    pub fn resolve_login(&mut self) -> LoginStorage {
        let open = |value: &str| crate::secret::open(value).value();
        self.token = self.token.take().or_else(|| self.token_sealed.as_deref().and_then(open));
        self.refresh_token = self.refresh_token.take().or_else(|| self.refresh_sealed.as_deref().and_then(open));
        if let Some(plain) = self.token_plain.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            if self.token.is_none() {
                self.token = Some(plain.to_owned());
            }
            return LoginStorage::NeedsSealing;
        }
        LoginStorage::Current
    }

    pub fn apply_login(&mut self, resolved: Self, sealed: Option<bool>) -> bool {
        self.token = resolved.token;
        self.refresh_token = resolved.refresh_token;
        self.token_expires = resolved.token_expires;
        let Some(sealed) = sealed else { return true };
        if !sealed {
            return false;
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.token_sealed = resolved.token_sealed;
            self.refresh_sealed = resolved.refresh_sealed;
            self.token_plain = resolved.token_plain;
        }
        self.write_config()
    }

    #[cfg(target_os = "macos")]
    pub fn seal_credentials(&mut self) -> bool {
        crate::session_file::save(&crate::session_file::directory(), &crate::session_file::SavedLogin {
            token: self.token.clone(),
            refresh: self.refresh_token.clone(),
            expires: self.token_expires,
        }).is_ok()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn seal_credentials(&mut self) -> bool {
        if let Some(t) = &self.token {
            self.token_sealed = crate::secret::seal("token", t);
        }
        self.refresh_sealed = self.refresh_token.as_deref().and_then(|token| crate::secret::seal("refresh", token));
        self.token_plain = if self.token_sealed.is_none() {
            self.token.clone()
        } else {
            None
        };
        true
    }

    pub fn save(&self) {
        self.write_config();
    }

    fn write_config(&self) -> bool {
        let dir = dir();
        let Ok(s) = serde_json::to_string_pretty(self) else { return false };
        let tmp = dir.join("config.json.tmp");
        let saved = std::fs::create_dir_all(&dir)
            .and_then(|_| write_private(&tmp, s.as_bytes()))
            .and_then(|_| std::fs::rename(&tmp, dir.join("config.json")));
        if let Err(error) = saved {
            let _ = std::fs::remove_file(&tmp);
            log(format!("не удалось записать настройки: {error}"));
            return false;
        }
        true
    }

    pub fn forget_login(&mut self) -> bool {
        #[cfg(target_os = "macos")]
        if crate::session_file::remove(&crate::session_file::directory()).is_err() {
            return false;
        }
        self.token = None;
        self.refresh_token = None;
        self.token_expires = None;
        #[cfg(not(target_os = "macos"))]
        {
            self.token_sealed = None;
            self.token_plain = None;
            self.refresh_sealed = None;
            crate::secret::forget("token");
            crate::secret::forget("refresh");
        }
        self.save();
        true
    }

}

/// На Linux без связки ключей токен лежит открытым текстом, поэтому файл сразу создаётся
/// только для владельца, а не получает права после записи.
fn write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
        // Права задаются только новому файлу, у оставшегося старого .tmp они бы не поменялись.
        let _ = std::fs::remove_file(path);
    }
    o.open(path)?.write_all(data)
}

/// Ошибка вместе с причинами. reqwest сам пишет только «error sending request», а что
/// случилось на самом деле (DNS, TLS, прокси, таймаут), лежит в source().
pub fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(x) = src {
        s.push_str(": ");
        s.push_str(&x.to_string());
        src = x.source();
    }
    s
}

/// Лог в log.txt папки настроек.
pub fn log(msg: impl AsRef<str>) {
    let msg = msg.as_ref();
    #[cfg(debug_assertions)]
    eprintln!("{msg}");
    let _ = std::fs::create_dir_all(dir());
    let path = dir().join("log.txt");
    if std::fs::metadata(&path).map(|m| m.len() > 512 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{t}] {msg}");
    }
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn restored_login_preserves_changes_to_settings() {
        let mut cfg: Config = serde_json::from_str(r#"{"token_sealed":"fixture","volume":0.5}"#).unwrap();
        #[cfg(not(target_os = "macos"))]
        assert!(cfg.has_saved_login());
        let mut resolved = cfg.clone();
        resolved.token = Some("access-fixture".into());
        resolved.refresh_token = Some("refresh-fixture".into());

        cfg.volume = 0.8;
        assert!(cfg.apply_login(resolved, None));

        assert_eq!(cfg.volume, 0.8);
        assert_eq!(cfg.token.as_deref(), Some("access-fixture"));
        assert_eq!(cfg.refresh_token.as_deref(), Some("refresh-fixture"));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(cfg.token_sealed.as_deref(), Some("fixture"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn settings_ignore_legacy_keychain_and_plaintext_credentials() {
        let mut cfg: Config = serde_json::from_str(r#"{"session_sealed":true,"token_sealed":"keyring:token","refresh_sealed":"keyring:refresh","token":"old-fixture","volume":0.3}"#).unwrap();
        assert!(cfg.token.is_none() && cfg.refresh_token.is_none());
        cfg.token = Some("new-fixture".into());
        let disk = serde_json::to_value(&cfg).unwrap();
        assert_eq!(cfg.volume, 0.3);
        for field in ["token", "refresh_token", "token_sealed", "refresh_sealed", "session_sealed"] {
            assert!(disk.get(field).is_none());
        }
    }
}
