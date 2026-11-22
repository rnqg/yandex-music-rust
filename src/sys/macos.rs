use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSModalResponseOK, NSOpenPanel, NSView, NSWorkspace,
};
use objc2_foundation::{NSArray, NSBundle, NSPoint, NSProcessInfo, NSRect, NSSize, NSString, NSURL};
use objc2_service_management::{SMAppService, SMAppServiceStatus};
use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

const BUNDLE_ID: &str = "local.music-rust.macos";

pub fn created(w: &slint::Window) -> bool {
    w.with_winit_window(|_| ()).is_some()
}

pub fn prepare(_: &slint::Window) {}

pub fn raise(w: &slint::Window) {
    w.set_minimized(false);
    w.with_winit_window(|window| window.focus_window());
}

pub fn scale(w: &slint::Window) -> f32 {
    w.with_winit_window(|window| window.scale_factor() as f32)
        .unwrap_or(1.0)
}

pub fn wave_frame_interval() -> std::time::Duration {
    let rate = MainThreadMarker::new()
        .and_then(|mtm| NSApplication::sharedApplication(mtm).keyWindow())
        .and_then(|window| window.screen())
        .map(|screen| screen.maximumFramesPerSecond())
        .filter(|rate| *rate > 0)
        .unwrap_or(60);
    std::time::Duration::from_secs_f64(1.0 / rate.clamp(24, 60) as f64)
}

pub fn animate_window_frame(
    w: &slint::Window,
    size: slint::LogicalSize,
    position: Option<slint::PhysicalPosition>,
) -> bool {
    if NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion() {
        return false;
    }
    w.with_winit_window(|window| {
        if window.fullscreen().is_some() {
            return None;
        }
        let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else { return None };
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let native = view.window()?;
        let current = native.frame();
        let mut frame = native.frameRectForContentRect(NSRect::new(NSPoint::ZERO, NSSize::new(f64::from(size.width), f64::from(size.height))));
        frame.origin.x = current.origin.x;
        frame.origin.y = current.origin.y + current.size.height - frame.size.height;
        if let Some(position) = position {
            let monitor = window.current_monitor()?;
            let origin = monitor.position();
            let bounds = monitor.size();
            if position.x < origin.x || position.x >= origin.x.saturating_add(bounds.width as i32)
                || position.y < origin.y || position.y >= origin.y.saturating_add(bounds.height as i32)
            {
                return None;
            }
            let outer = window.outer_position().ok()?;
            let scale = window.scale_factor();
            frame.origin.x += (f64::from(position.x) - f64::from(outer.x)) / scale;
            frame.origin.y -= (f64::from(position.y) - f64::from(outer.y)) / scale;
        }
        native.setFrame_display_animate(frame, true, true);
        Some(())
    }).flatten().is_some()
}

pub fn work_area(w: &slint::Window) -> (i32, i32, i32, i32) {
    let area = w.with_winit_window(|window| {
        let monitor = window.current_monitor()?;
        let pos = monitor.position();
        let size = monitor.size();
        let screen = MainThreadMarker::new()
            .and_then(|mtm| NSApplication::sharedApplication(mtm).keyWindow())
            .and_then(|window| window.screen());
        let (left, top, right, bottom) = screen.map_or((0, 0, 0, 0), |screen| {
            let frame = screen.frame();
            let visible = screen.visibleFrame();
            let scale = monitor.scale_factor();
            (
                ((visible.origin.x - frame.origin.x) * scale).round() as i32,
                ((frame.origin.y + frame.size.height - visible.origin.y - visible.size.height)
                    * scale)
                    .round() as i32,
                ((frame.origin.x + frame.size.width - visible.origin.x - visible.size.width)
                    * scale)
                    .round() as i32,
                ((visible.origin.y - frame.origin.y) * scale).round() as i32,
            )
        });
        Some((
            pos.x + left,
            pos.y + top,
            pos.x + size.width as i32 - right,
            pos.y + size.height as i32 - bottom,
        ))
    });
    area.flatten().unwrap_or_else(|| {
        let size = w.size();
        (0, 0, size.width as i32, size.height as i32)
    })
}

fn pick_path(title: &str, folder: bool, start: Option<&str>) -> Option<String> {
    let panel = NSOpenPanel::openPanel(MainThreadMarker::new()?);
    panel.setMessage(Some(&NSString::from_str(title)));
    panel.setCanChooseDirectories(folder);
    panel.setCanChooseFiles(!folder);
    panel.setAllowsMultipleSelection(false);
    if let Some(path) = start.filter(|path| Path::new(path).is_dir()) {
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath_isDirectory(
            &NSString::from_str(path),
            true,
        )));
    }
    if !folder {
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[
            NSString::from_str("jpg"),
            NSString::from_str("jpeg"),
            NSString::from_str("png"),
        ])));
    }
    (panel.runModal() == NSModalResponseOK)
        .then(|| panel.URL()?.path().map(|path| path.to_string()))
        .flatten()
}

pub fn pick_image(_: &slint::Window, title: &str) -> Option<String> {
    pick_path(title, false, None)
}

pub fn pick_folder(_: &slint::Window, title: &str, start: &str) -> Option<String> {
    pick_path(title, true, Some(start))
}

pub fn open_url(url: &str) {
    if let Ok(mut child) = Command::new("/usr/bin/open")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || child.wait());
    }
}

pub fn release_memory() {
    skia_safe::graphics::purge_font_cache();
}

pub fn system_light_theme() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return true;
    };
    let names = unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
    let appearance = NSApplication::sharedApplication(mtm).effectiveAppearance();
    !appearance
        .bestMatchFromAppearancesWithNames(&names)
        .is_some_and(|name| &*name == unsafe { NSAppearanceNameDarkAqua })
}

fn home() -> PathBuf {
    std::env::home_dir().expect("macOS home directory")
}

pub fn config_base() -> PathBuf {
    home().join("Library/Application Support")
}

pub fn cache_base() -> PathBuf {
    home().join("Library/Caches")
}

pub fn music_dir() -> PathBuf {
    home().join("Music")
}

pub fn hostname() -> Option<String> {
    let hostname = NSProcessInfo::processInfo().hostName().to_string();
    (!hostname.is_empty()).then_some(hostname)
}

pub fn copy_text(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text);
    }
}

fn login_item() -> Option<objc2::rc::Retained<SMAppService>> {
    let id = NSBundle::mainBundle().bundleIdentifier()?;
    (id.to_string() == BUNDLE_ID).then(|| unsafe { SMAppService::mainAppService() })
}

pub fn autostart_enabled() -> bool {
    login_item().is_some_and(|item| {
        matches!(
            unsafe { item.status() },
            SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval
        )
    })
}

pub fn set_autostart(on: bool) -> bool {
    let Some(item) = login_item() else {
        return false;
    };
    if on == autostart_enabled() {
        return true;
    }
    let result = unsafe {
        if on {
            item.registerAndReturnError()
        } else {
            item.unregisterAndReturnError()
        }
    };
    if let Err(error) = result {
        crate::config::log(format!("SMAppService: {error}"));
        return false;
    }
    true
}

pub fn refresh_autostart() {}

struct Instance {
    socket: PathBuf,
    _lock: File,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
    }
}

static INSTANCE: Mutex<Option<Instance>> = Mutex::new(None);
static LISTENER: Mutex<Option<UnixDatagram>> = Mutex::new(None);

fn socket_path() -> PathBuf {
    std::env::temp_dir().join("yamusic-rs.sock")
}

pub fn claim_instance() -> bool {
    let path = socket_path();
    let result = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path.with_extension("lock"));
    let Ok(lock) = result else {
        crate::config::log("не удалось открыть блокировку второго экземпляра");
        return true;
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return false,
        Err(error) => {
            crate::config::log(format!("блокировка экземпляра: {error}"));
            return true;
        }
    }
    let _ = std::fs::remove_file(&path);
    match UnixDatagram::bind(&path) {
        Ok(listener) => *LISTENER.lock().unwrap() = Some(listener),
        Err(error) => crate::config::log(format!("сокет экземпляра: {error}")),
    }
    *INSTANCE.lock().unwrap() = Some(Instance {
        socket: path,
        _lock: lock,
    });
    true
}

pub fn wake_existing() {
    let Ok(socket) = UnixDatagram::unbound() else {
        return;
    };
    for _ in 0..20 {
        if socket.send_to(b"s", socket_path()).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

pub fn on_wake(f: impl Fn() + Send + 'static) {
    let Some(listener) = LISTENER.lock().unwrap().take() else {
        return;
    };
    std::thread::spawn(move || {
        let mut msg = [0; 1];
        while let Ok(size) = listener.recv(&mut msg) {
            if size == 1 && msg[0] == b's' {
                f();
            }
        }
    });
}

pub fn cleanup() {
    INSTANCE.lock().unwrap().take();
}
