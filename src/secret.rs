//! Где хранить токен. Windows: шифрование DPAPI, расшифровать может только та же учётная
//! запись на том же компьютере, в config.json лежит base64 шифротекста. Linux: связка ключей
//! пользователя через Secret Service, в config.json только имя записи.

#[cfg(windows)]
use base64::Engine as _;

/// Дополнительная «соль» DPAPI: другие программы того же пользователя без неё не расшифруют.
/// Старое имя оставлено намеренно: с другой солью сохранённые токены перестанут читаться.
#[cfg(windows)]
const ENTROPY: &[u8] = b"yamusic-lite token v1";

#[cfg(windows)]
mod dpapi {
    use super::ENTROPY;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(b: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: b.len() as u32, pbData: b.as_ptr() as *mut u8 }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
        unsafe { LocalFree(out.pbData as _) };
        v
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(&input, std::ptr::null(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let (input, entropy) = (blob(data), blob(ENTROPY));
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(&input, std::ptr::null_mut(), &entropy, std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }
}

/// Связка ключей Linux через Secret Service (GNOME Keyring, KWallet, KeePassXC). Секрет лежит
/// там, в config.json остаётся только имя записи. Сессия шифрованная (Диффи — Хеллман и AES),
/// так что по шине D-Bus секрет открытым не идёт.
#[cfg(target_os = "linux")]
mod keyring {
    use crate::config::log;
    use secret_service::blocking::SecretService;
    use secret_service::EncryptionType;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// Что уже лежит в связке: те же значения повторно не пишем. Конфиг сохраняется на каждое
    /// изменение настроек, а запись в связку — это запрос по D-Bus.
    static STORED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    /// Службы нет (нет шины или демона): до конца работы больше не пытаемся.
    static MISSING: AtomicBool = AtomicBool::new(false);

    fn attributes(name: &str) -> HashMap<&str, &str> {
        HashMap::from([("application", "yamusic-rs"), ("secret", name)])
    }

    fn label(name: &str) -> String {
        let what = if name == "refresh" { "токен обновления" } else { "токен входа" };
        format!("{}: {what}", env!("APP_NAME"))
    }

    fn with<R>(f: impl FnOnce(&SecretService) -> Result<R, secret_service::Error>) -> Option<R> {
        if MISSING.load(Ordering::Relaxed) {
            return None;
        }
        let ss = match SecretService::connect(EncryptionType::Dh) {
            Ok(ss) => ss,
            Err(e) => {
                MISSING.store(true, Ordering::Relaxed);
                log(format!("связка ключей недоступна, токен останется в config.json: {e}"));
                return None;
            }
        };
        f(&ss).map_err(|e| log(format!("связка ключей: {e}"))).ok()
    }

    fn cached(f: impl FnOnce(&mut HashMap<String, String>)) {
        f(STORED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new));
    }

    pub fn store(name: &str, secret: &str) -> bool {
        let mut same = false;
        cached(|m| same = m.get(name).is_some_and(|v| v == secret));
        if same {
            return true;
        }
        let saved = with(|ss| {
            let c = ss.get_default_collection().or_else(|_| ss.get_any_collection())?;
            c.ensure_unlocked()?;
            c.create_item(&label(name), attributes(name), secret.as_bytes(), true, "text/plain")?;
            Ok(())
        });
        if saved.is_some() {
            cached(|m| {
                m.insert(name.into(), secret.into());
            });
        }
        saved.is_some()
    }

    pub fn load(name: &str) -> Option<String> {
        let secret = with(|ss| {
            let found = ss.search_items(attributes(name))?;
            let Some(item) = found.unlocked.first().or(found.locked.first()) else { return Ok(None) };
            item.ensure_unlocked()?;
            Ok(String::from_utf8(item.get_secret()?).ok())
        })??;
        cached(|m| {
            m.insert(name.into(), secret.clone());
        });
        Some(secret)
    }

    pub fn remove(name: &str) {
        let mut known = false;
        cached(|m| known = m.remove(name).is_some());
        if known {
            with(|ss| ss.search_items(attributes(name))?.unlocked.iter().try_for_each(|i| i.delete()));
        }
    }
}

/// Префикс в config.json: секрет лежит в связке ключей, а не рядом.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const IN_KEYRING: &str = "keyring:";

pub enum Opened<T = String> {
    Value(T),
    Missing,
    #[cfg(target_os = "macos")]
    Unavailable,
}

impl<T> Opened<T> {
    pub fn value(self) -> Option<T> {
        if let Self::Value(value) = self { Some(value) } else { None }
    }
}

#[cfg(target_os = "macos")]
const LOGIN_ITEM: &str = "oauth-session";

#[cfg(target_os = "macos")]
#[derive(serde::Deserialize, serde::Serialize)]
pub struct SavedLogin {
    pub token: Option<String>,
    pub refresh: Option<String>,
    pub expires: Option<u64>,
}

#[cfg(target_os = "macos")]
pub fn read_login() -> Opened<SavedLogin> {
    match keyring::load(LOGIN_ITEM) {
        Opened::Value(value) => serde_json::from_str(&value).map_or(Opened::Unavailable, Opened::Value),
        Opened::Missing => Opened::Missing,
        Opened::Unavailable => Opened::Unavailable,
    }
}

#[cfg(target_os = "macos")]
pub fn store_login(login: &SavedLogin) -> bool {
    serde_json::to_string(login).is_ok_and(|value| keyring::store(LOGIN_ITEM, &value))
}

#[cfg(target_os = "macos")]
pub fn forget_login() {
    keyring::remove(LOGIN_ITEM);
}

#[cfg(target_os = "macos")]
mod keyring {
    use super::Opened;
    use security_framework::base::Result;
    use security_framework::os::macos::keychain::SecKeychain;
    use std::sync::Mutex;

    static ACCESS: Mutex<()> = Mutex::new(());
    const EXISTING_SERVICE: &str = "io.github.spolzermod.yamusic-rs.macos";
    const ITEM_NOT_FOUND: i32 = -25300;
    const DUPLICATE_ITEM: i32 = -25299;
    const AUTH_FAILED: i32 = -25293;
    const INTERACTION_NOT_ALLOWED: i32 = -25308;

    fn with<R>(allow_prompt: bool, f: impl Fn(&SecKeychain) -> Result<R>) -> Result<R> {
        let _access = ACCESS.lock().unwrap();
        let (keychain, result) = {
            let _without_dialogs = SecKeychain::disable_user_interaction()?;
            let keychain = SecKeychain::default()?;
            let result = f(&keychain);
            (keychain, result)
        };
        match result {
            Err(error)
                if allow_prompt
                    && matches!(error.code(), AUTH_FAILED | INTERACTION_NOT_ALLOWED | DUPLICATE_ITEM) =>
            {
                f(&keychain)
            }
            result => result,
        }
    }

    pub fn store(name: &str, secret: &str) -> bool {
        with(true, |keychain| keychain.set_generic_password(EXISTING_SERVICE, name, secret.as_bytes()))
            .map_err(|error| crate::config::log(format!("Связка ключей: {error}")))
            .is_ok()
    }

    pub fn load(name: &str) -> Opened {
        match with(true, |keychain| {
            let (password, _) = keychain.find_generic_password(EXISTING_SERVICE, name)?;
            Ok(password.to_vec())
        }) {
            Ok(bytes) => String::from_utf8(bytes).map_or(Opened::Missing, Opened::Value),
            Err(error) if error.code() == ITEM_NOT_FOUND => Opened::Missing,
            Err(_) => Opened::Unavailable,
        }
    }

    pub fn remove(name: &str) {
        let result = with(false, |keychain| {
            let (_, item) = keychain.find_generic_password(EXISTING_SERVICE, name)?;
            item.delete();
            Ok(())
        });
        if let Err(error) = result {
            if error.code() != ITEM_NOT_FOUND {
                crate::config::log(format!("Связка ключей: {error}"));
            }
        }
    }
}

/// Спрятать секрет для хранения в конфиге. `name` различает секреты в связке ключей Linux,
/// DPAPI на Windows он не нужен. None — спрятать негде.
#[cfg(not(target_os = "macos"))]
pub fn seal(name: &str, plain: &str) -> Option<String> {
    #[cfg(windows)]
    {
        let _ = name;
        dpapi::protect(plain.as_bytes()).map(|b| base64::engine::general_purpose::STANDARD.encode(b))
    }
    #[cfg(target_os = "linux")]
    {
        keyring::store(name, plain).then(|| format!("{IN_KEYRING}{name}"))
    }
}

pub fn open(sealed: &str) -> Opened {
    #[cfg(windows)]
    {
        base64::engine::general_purpose::STANDARD.decode(sealed.trim())
            .ok()
            .and_then(|raw| dpapi::unprotect(&raw))
            .and_then(|raw| String::from_utf8(raw).ok())
            .map_or(Opened::Missing, Opened::Value)
    }
    #[cfg(target_os = "linux")]
    {
        sealed.trim().strip_prefix(IN_KEYRING)
            .and_then(keyring::load)
            .map_or(Opened::Missing, Opened::Value)
    }
    #[cfg(target_os = "macos")]
    {
        sealed.trim().strip_prefix(IN_KEYRING).map_or(Opened::Missing, keyring::load)
    }
}

/// Секрет больше не нужен (вышли из аккаунта). DPAPI ничего не хранит, связку ключей чистим.
pub fn forget(name: &str) {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    keyring::remove(name);
    #[cfg(windows)]
    let _ = name;
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn roundtrip() {
        let s = super::seal("token", "y0_secret").unwrap();
        assert!(!s.contains("y0_secret"));
        assert_eq!(super::open(&s).value().as_deref(), Some("y0_secret"));
        assert!(super::open("AAAA").value().is_none());
    }
}
