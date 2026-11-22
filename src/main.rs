//! Яндекс Музыка Rust — неофициальный клиент Яндекс Музыки. Точка входа: один экземпляр
//! на пользователя, выбор рендерера, запуск окна.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod assets;
mod auth;
mod client;
mod config;
mod discord;
mod eq;
mod flac;
mod gui;
mod history;
mod i18n;
mod images;
mod landing;
#[cfg(target_os = "macos")]
mod media;
#[cfg(target_os = "linux")]
mod mpris;
mod net;
mod palette;
mod player;
mod secret;
#[cfg(target_os = "macos")]
mod spectrum;
mod stream;
#[cfg_attr(windows, path = "sys/windows.rs")]
#[cfg_attr(target_os = "linux", path = "sys/linux.rs")]
#[cfg_attr(target_os = "macos", path = "sys/macos.rs")]
mod sys;
mod tags;
#[cfg(windows)]
mod tray;
mod update;
mod yandex_id;

fn main() -> Result<(), slint::PlatformError> {
    #[cfg(target_os = "linux")]
    sys::audio_identity();
    // После обновления старая версия ещё закрывается: ждём её до 15 секунд.
    let after_update = std::env::args().any(|a| a == update::AFTER_UPDATE);
    let mut claimed = sys::claim_instance();
    for _ in 0..60 {
        if claimed || !after_update {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        claimed = sys::claim_instance();
    }
    if !claimed {
        // Второй запуск просто показывает уже открытое окно.
        sys::wake_existing();
        return Ok(());
    }
    let cfg = config::Config::load_settings();
    let renderer = if cfg!(target_os = "macos") && cfg.mode != "lite" { "skia" } else { "skia-software" };
    let backend = slint::BackendSelector::new().backend_name("winit".into()).renderer_name(renderer.into());
    #[cfg(target_os = "macos")]
    let backend = {
        use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;

        backend.with_winit_window_attributes_hook(|window| {
            window.with_title_hidden(true).with_titlebar_transparent(true).with_fullsize_content_view(true)
        })
    };
    backend.select()?;
    // По app_id (Wayland) и WM_CLASS (X11) окружение находит .desktop и значок.
    #[cfg(target_os = "linux")]
    slint::set_xdg_app_id(env!("CARGO_PKG_NAME"))?;
    update::cleanup();
    sys::refresh_autostart();
    // Без трея на Linux спрятанное окно было бы не вернуть.
    let start_hidden = cfg!(windows) && std::env::args().any(|a| a == "--tray");
    gui::run(start_hidden, cfg)
}
