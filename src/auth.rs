//! Вход в аккаунт Яндекса по коду устройства: приложение показывает код,
//! пользователь вводит его на ya.ru/device в любом браузере, приложение получает токен.

use crate::client::{OAUTH_BASE, OAUTH_CLIENT_ID, OAUTH_CLIENT_SECRET};
use crate::{api::{Account, AccountError}, config::Config};
use serde_json::Value;
use std::time::Duration;

/// Права токена, которые просим при входе.
const SCOPE: &str = "music:content music:read music:write";

pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub interval: u64,
    pub expires_in: u64,
}

pub struct Token {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_in: Option<u64>,
}

#[derive(Debug)]
pub enum RefreshError {
    Rejected,
    Unavailable(String),
}

pub enum Poll {
    /// Пользователь ещё не ввёл код.
    Pending,
    Done(Token),
    /// Код истёк или вход отклонён — нужен новый код.
    Failed(String),
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("http client")
}

fn post(form: &[(&str, &str)], path: &str) -> Result<Value, String> {
    let resp = client().post(format!("{OAUTH_BASE}{path}")).form(form).send().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| format!("сеть: {}", crate::config::chain(&e)))?;
    serde_json::from_str(&text).map_err(|_| format!("Некорректный ответ OAuth (HTTP {status})"))
}

/// Постоянный id устройства: Яндекс показывает его в списке устройств аккаунта.
pub fn device_id() -> String {
    let path = crate::config::dir().join("device");
    if let Ok(id) = std::fs::read_to_string(&path) {
        let id = id.trim().to_string();
        if id.len() == 32 {
            return id;
        }
    }
    let seed = format!("{:?}{}{}", std::time::SystemTime::now(), std::process::id(), crate::sys::hostname().unwrap_or_default());
    let id: String = {
        use md5::{Digest, Md5};
        Md5::digest(seed.as_bytes()).iter().map(|x| format!("{x:02x}")).collect()
    };
    let _ = std::fs::create_dir_all(crate::config::dir());
    let _ = std::fs::write(&path, &id);
    id
}

pub fn device_code() -> Result<DeviceCode, String> {
    let id = device_id();
    let host = crate::sys::hostname().unwrap_or_else(|| "PC".into());
    // Это имя Яндекс показывает при вводе кода и в списке устройств аккаунта.
    let name = format!("{} ({host})", env!("APP_NAME"));
    let v = post(
        &[("client_id", OAUTH_CLIENT_ID), ("device_id", &id), ("device_name", &name), ("scope", SCOPE)],
        "/device/code",
    )?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let n = |k: &str| v.get(k).and_then(|x| x.as_u64());
    match (s("device_code"), s("user_code")) {
        (Some(device_code), Some(user_code)) => Ok(DeviceCode {
            device_code,
            user_code,
            interval: n("interval").unwrap_or(5).max(1),
            expires_in: n("expires_in").unwrap_or(300),
        }),
        _ => Err(s("error_description").unwrap_or_else(|| "Яндекс не выдал код входа".into())),
    }
}

fn token_from(v: &Value) -> Option<Token> {
    Some(Token {
        access: v.get("access_token")?.as_str()?.to_string(),
        refresh: v.get("refresh_token").and_then(|x| x.as_str()).map(String::from),
        expires_in: v.get("expires_in").and_then(|x| x.as_u64()),
    })
}

pub fn poll(device_code: &str) -> Poll {
    let v = match post(
        &[
            ("grant_type", "device_code"),
            ("code", device_code),
            ("client_id", OAUTH_CLIENT_ID),
            ("client_secret", OAUTH_CLIENT_SECRET),
        ],
        "/token",
    ) {
        Ok(v) => v,
        // Сеть мигнула — спросим ещё раз на следующем шаге.
        Err(_) => return Poll::Pending,
    };
    if let Some(t) = token_from(&v) {
        return Poll::Done(t);
    }
    match v.get("error").and_then(|e| e.as_str()) {
        Some("authorization_pending") | Some("slow_down") => Poll::Pending,
        _ => Poll::Failed(
            v.get("error_description").and_then(|e| e.as_str()).unwrap_or("вход не удался").to_string(),
        ),
    }
}

/// Отозвать токен на сервере. Яндекс отзывает только токены, выданные с device_id.
pub fn revoke(access: &str) -> Result<(), String> {
    let v = post(
        &[("access_token", access), ("client_id", OAUTH_CLIENT_ID), ("client_secret", OAUTH_CLIENT_SECRET)],
        "/revoke_token",
    )?;
    if v.get("status").and_then(|s| s.as_str()) == Some("ok") {
        Ok(())
    } else {
        Err(v.get("error_description").and_then(|e| e.as_str()).unwrap_or("Яндекс не отозвал токен").to_string())
    }
}

fn refresh(refresh_token: &str) -> Result<Token, RefreshError> {
    let v = post(
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", OAUTH_CLIENT_ID),
            ("client_secret", OAUTH_CLIENT_SECRET),
        ],
        "/token",
    ).map_err(RefreshError::Unavailable)?;
    if let Some(token) = token_from(&v) {
        return Ok(token);
    }
    if v.get("error").and_then(Value::as_str) == Some("invalid_grant") {
        return Err(RefreshError::Rejected);
    }
    let reason = v.get("error").and_then(Value::as_str).unwrap_or("нет токена в ответе");
    Err(RefreshError::Unavailable(format!("Не удалось продлить вход: {reason}")))
}

pub fn restore_account(cfg: &mut Config) -> (bool, Result<Account, AccountError>) {
    prepare_account(cfg, crate::config::now_secs(), refresh, |token| crate::api::Api::new(token).account())
}

fn prepare_account(
    cfg: &mut Config,
    now: u64,
    exchange: impl Fn(&str) -> Result<Token, RefreshError>,
    account: impl Fn(&str) -> Result<Account, AccountError>,
) -> (bool, Result<Account, AccountError>) {
    let refresh_first = cfg.token.is_none()
        || cfg.token_expires.is_some_and(|expires| expires <= now.saturating_add(30 * 86400));
    let early = cfg.refresh_token.as_deref().filter(|_| refresh_first).map(&exchange);
    let checked = match early {
        Some(Ok(token)) => return (true, account(accept_refresh(cfg, token, now))),
        _ => cfg.token.as_deref().ok_or(AccountError::Unauthorized).and_then(&account),
    };
    if !matches!(checked, Err(AccountError::Unauthorized)) {
        return (false, checked);
    }
    match early.or_else(|| cfg.refresh_token.as_deref().map(exchange)) {
        Some(Ok(token)) => (true, account(accept_refresh(cfg, token, now))),
        Some(Err(RefreshError::Unavailable(error))) => (false, Err(AccountError::Unavailable(error))),
        Some(Err(RefreshError::Rejected)) | None => (false, checked),
    }
}

fn accept_refresh(cfg: &mut Config, token: Token, now: u64) -> &str {
    let access = cfg.token.insert(token.access);
    if let Some(refresh) = token.refresh {
        cfg.refresh_token = Some(refresh);
    }
    cfg.token_expires = token.expires_in.map(|expires| now.saturating_add(expires));
    access
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account { uid: "42".into(), login: "fixture".into(), has_plus: true }
    }

    fn renewed(refresh: Option<&str>) -> Token {
        Token { access: "new-access".into(), refresh: refresh.map(str::to_owned), expires_in: Some(60) }
    }

    #[test]
    fn expired_access_is_replaced_before_checking_account() {
        let mut cfg = Config::default();
        cfg.token = Some("expired".into());
        cfg.refresh_token = Some("saved-refresh".into());
        cfg.token_expires = Some(10);
        let (changed, result) = prepare_account(&mut cfg, 20, |key| {
            assert_eq!(key, "saved-refresh");
            Ok(renewed(Some("rotated-refresh")))
        }, |key| {
            assert_eq!(key, "new-access");
            Ok(account())
        });
        assert!(changed);
        assert_eq!(result.ok().unwrap().uid, "42");
        assert_eq!(cfg.token.as_deref(), Some("new-access"));
        assert_eq!(cfg.refresh_token.as_deref(), Some("rotated-refresh"));
        assert_eq!(cfg.token_expires, Some(80));
    }

    #[test]
    fn unauthorized_access_without_expiration_renews_once() {
        let mut cfg = Config::default();
        cfg.token = Some("expired".into());
        cfg.refresh_token = Some("saved-refresh".into());
        let attempts = std::cell::Cell::new(0);
        let (changed, result) = prepare_account(&mut cfg, 20, |_| {
            attempts.set(attempts.get() + 1);
            Ok(renewed(None))
        }, |key| {
            if key == "expired" { Err(AccountError::Unauthorized) } else { Ok(account()) }
        });
        assert!(changed && result.is_ok());
        assert_eq!(attempts.get(), 1);
        assert_eq!(cfg.refresh_token.as_deref(), Some("saved-refresh"));
    }

    #[test]
    fn temporary_refresh_error_preserves_saved_login_for_retry() {
        let mut cfg = Config::default();
        cfg.refresh_token = Some("saved-refresh".into());
        let (changed, result) = prepare_account(&mut cfg, 20, |_| {
            Err(RefreshError::Unavailable("offline".into()))
        }, |_| panic!("there is no access token"));
        assert!(!changed);
        assert!(matches!(result, Err(AccountError::Unavailable(error)) if error == "offline"));
        assert_eq!(cfg.refresh_token.as_deref(), Some("saved-refresh"));
    }

    #[test]
    fn rejected_refresh_can_fall_back_to_current_access() {
        let mut cfg = Config::default();
        cfg.token = Some("valid".into());
        cfg.refresh_token = Some("revoked-refresh".into());
        cfg.token_expires = Some(60);
        let (changed, result) = prepare_account(&mut cfg, 20, |_| Err(RefreshError::Rejected), |key| {
            assert_eq!(key, "valid");
            Ok(account())
        });
        assert!(!changed && result.is_ok());
        assert_eq!(cfg.token.as_deref(), Some("valid"));
    }
}
