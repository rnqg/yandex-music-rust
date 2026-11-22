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
    #[cfg(target_os = "macos")]
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    session_sealed: bool,
    /// Токены под DPAPI или в связке ключей Linux (src/secret.rs).
    #[serde(skip_serializing_if = "Option::is_none")]
    token_sealed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_sealed: Option<String>,
    /// Токен открытым текстом: старый формат или Linux без связки ключей. Как только спрятать есть
    /// куда, при запуске переносится туда.
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
    pub eq_on: bool,
    pub eq_preset: usize,
    pub eq_preamp: f32,
    pub eq_gains: [f32; 10],
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
            #[cfg(target_os = "macos")]
            session_sealed: false,
            token_sealed: None,
            refresh_sealed: None,
            token_plain: None,
            token_expires: None,
            volume: 0.6,
            discord_enabled: true,
            discord_app_id: String::new(),
            theme: "system".into(),
            quality: 0,
            eq_on: false,
            eq_preset: 1,
            eq_preamp: 1.0,
            eq_gains: [0.0; 10],
            lang: "ru".into(),
            navbar_collapsed: false,
            mode: "smooth".into(),
            download_dir: String::new(),
            download_flac: false,
            hidden_blocks: Vec::new(),
            check_updates: true,
            allow_explicit: true,
            window_pos: None,
            window_size: None,
            window_maximized: false,
            mini_pos: None,
        }
    }
}

pub enum LoginStorage {
    Current,
    NeedsSealing,
    #[cfg(target_os = "macos")]
    Recovered,
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

    pub fn has_saved_login(&self) -> bool {
        #[cfg(target_os = "macos")]
        if self.session_sealed {
            return true;
        }
        self.token_sealed.is_some() || self.refresh_sealed.is_some()
            || self.token_plain.as_deref().is_some_and(|token| !token.trim().is_empty())
    }

    pub fn resolve_login(&mut self) -> LoginStorage {
        self.resolve_stored_login(
            #[cfg(target_os = "macos")]
            crate::secret::read_login,
            crate::secret::open,
        )
    }

    fn resolve_stored_login(
        &mut self,
        #[cfg(target_os = "macos")] read_login: impl FnOnce() -> crate::secret::Opened<crate::secret::SavedLogin>,
        open: impl Fn(&str) -> crate::secret::Opened,
    ) -> LoginStorage {
        use crate::secret::Opened;
        #[cfg(target_os = "macos")]
        if self.token.is_none() && self.refresh_token.is_none()
            && (self.session_sealed || self.token_sealed.is_some() || self.refresh_sealed.is_some())
        {
            match read_login() {
                Opened::Value(saved) => {
                    self.token = saved.token;
                    self.refresh_token = saved.refresh;
                    self.token_expires = saved.expires;
                    if self.session_sealed {
                        return LoginStorage::Current;
                    }
                    self.mark_session_sealed();
                    return LoginStorage::Recovered;
                }
                Opened::Unavailable => return LoginStorage::Current,
                Opened::Missing => {}
            }
        }
        let stored = self.token.take().map(Opened::Value)
            .unwrap_or_else(|| self.token_sealed.as_deref().map_or(Opened::Missing, &open));
        #[cfg(target_os = "macos")]
        let can_read_refresh = !matches!(stored, Opened::Unavailable);
        #[cfg(not(target_os = "macos"))]
        let can_read_refresh = true;
        self.token = stored.value();
        let refresh = if can_read_refresh {
            self.refresh_token.take().map(Opened::Value)
                .unwrap_or_else(|| self.refresh_sealed.as_deref().map_or(Opened::Missing, open))
        } else {
            Opened::Missing
        };
        #[cfg(target_os = "macos")]
        let legacy_read = can_read_refresh && !matches!(refresh, Opened::Unavailable);
        self.refresh_token = refresh.value();
        let plain = self.token_plain.as_deref().map(str::trim).filter(|t| !t.is_empty());
        if self.token.is_none() {
            self.token = plain.map(str::to_owned);
        }
        #[cfg(target_os = "macos")]
        if !self.session_sealed && legacy_read && (self.token.is_some() || self.refresh_token.is_some()) {
            return LoginStorage::NeedsSealing;
        }
        #[cfg(not(target_os = "macos"))]
        if plain.is_some() {
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
        #[cfg(target_os = "macos")]
        let remove_legacy = resolved.session_sealed && !self.session_sealed
            && (self.token_sealed.is_some() || self.refresh_sealed.is_some());
        #[cfg(target_os = "macos")]
        {
            self.session_sealed = resolved.session_sealed;
        }
        self.token_sealed = resolved.token_sealed;
        self.refresh_sealed = resolved.refresh_sealed;
        self.token_plain = resolved.token_plain;
        let saved = self.write_config();
        #[cfg(target_os = "macos")]
        if saved && remove_legacy {
            std::thread::spawn(|| {
                crate::secret::forget("token");
                crate::secret::forget("refresh");
            });
        }
        saved
    }

    #[cfg(target_os = "macos")]
    fn mark_session_sealed(&mut self) {
        self.session_sealed = true;
        self.token_sealed = None;
        self.refresh_sealed = None;
        self.token_plain = None;
    }

    #[cfg(target_os = "macos")]
    pub fn seal_credentials(&mut self) -> bool {
        let saved = crate::secret::store_login(&crate::secret::SavedLogin {
            token: self.token.clone(),
            refresh: self.refresh_token.clone(),
            expires: self.token_expires,
        });
        if saved {
            self.mark_session_sealed();
        }
        saved
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

    pub fn forget_login(&mut self) {
        self.token = None;
        self.token_sealed = None;
        self.token_plain = None;
        self.refresh_token = None;
        self.refresh_sealed = None;
        self.token_expires = None;
        #[cfg(target_os = "macos")]
        {
            self.session_sealed = false;
            crate::secret::forget_login();
        }
        crate::secret::forget("token");
        crate::secret::forget("refresh");
        self.save();
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
        assert!(cfg.has_saved_login());
        let mut resolved = cfg.clone();
        resolved.token = Some("access-fixture".into());
        resolved.refresh_token = Some("refresh-fixture".into());

        cfg.volume = 0.8;
        assert!(cfg.apply_login(resolved, None));

        assert_eq!(cfg.volume, 0.8);
        assert_eq!(cfg.token.as_deref(), Some("access-fixture"));
        assert_eq!(cfg.refresh_token.as_deref(), Some("refresh-fixture"));
        assert_eq!(cfg.token_sealed.as_deref(), Some("fixture"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn legacy_records_are_retained_until_combined_record_is_saved() {
        use super::LoginStorage;
        use crate::secret::Opened;
        let mut cfg: Config = serde_json::from_str(
            r#"{"token_sealed":"keyring:token","refresh_sealed":"keyring:refresh","token_expires":123}"#,
        ).unwrap();
        let mut resolved = cfg.clone();
        let storage = resolved.resolve_stored_login(|| Opened::Missing, |marker| match marker {
            "keyring:token" => Opened::Value("legacy-access".into()),
            "keyring:refresh" => Opened::Value("legacy-refresh".into()),
            _ => panic!("unexpected record"),
        });
        assert!(matches!(storage, LoginStorage::NeedsSealing));
        assert!(!cfg.apply_login(resolved, Some(false)));
        assert_eq!(cfg.token.as_deref(), Some("legacy-access"));
        assert_eq!(cfg.refresh_token.as_deref(), Some("legacy-refresh"));
        assert_eq!(cfg.token_sealed.as_deref(), Some("keyring:token"));
        assert_eq!(cfg.refresh_sealed.as_deref(), Some("keyring:refresh"));
        assert!(!cfg.session_sealed);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn combined_record_is_recovered_without_reopening_legacy_records() {
        use super::LoginStorage;
        use crate::secret::{Opened, SavedLogin};
        let mut cfg: Config = serde_json::from_str(
            r#"{"token_sealed":"keyring:token","refresh_sealed":"keyring:refresh","token_expires":123}"#,
        ).unwrap();
        let storage = cfg.resolve_stored_login(|| Opened::Value(SavedLogin {
            token: Some("combined-access".into()),
            refresh: Some("combined-refresh".into()),
            expires: Some(456),
        }), |_| panic!("legacy record should not be opened"));
        assert!(matches!(storage, LoginStorage::Recovered));
        assert_eq!(cfg.token_expires, Some(456));
        let disk = serde_json::to_value(&cfg).unwrap();
        assert_eq!(disk["session_sealed"], true);
        for field in ["token", "refresh_token", "token_sealed", "refresh_sealed"] {
            assert!(disk.get(field).is_none());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn denied_session_does_not_request_legacy_records() {
        use super::LoginStorage;
        use crate::secret::Opened;
        let mut cfg: Config = serde_json::from_str(r#"{"session_sealed":true,"token_sealed":"keyring:token"}"#).unwrap();
        let storage = cfg.resolve_stored_login(|| Opened::Unavailable, |_| panic!("unexpected legacy read"));
        assert!(matches!(storage, LoginStorage::Current));
        assert!(cfg.token.is_none() && cfg.refresh_token.is_none());
        assert!(cfg.has_saved_login());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plaintext_legacy_login_can_be_migrated_without_a_keychain_read() {
        use super::LoginStorage;
        let mut cfg: Config = serde_json::from_str(r#"{"token":"legacy-fixture"}"#).unwrap();
        let storage = cfg.resolve_stored_login(
            || panic!("no combined record is configured"),
            |_| panic!("no legacy record is configured"),
        );
        assert!(matches!(storage, LoginStorage::NeedsSealing));
        assert_eq!(cfg.token.as_deref(), Some("legacy-fixture"));
    }
}
