use rustix::fs::{fstat, open, openat, renameat, unlinkat, AtFlags, FileType, Mode, OFlags};
use rustix::process::geteuid;
use serde::{Deserialize, Serialize};
use std::fs::{DirBuilder, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SESSION: &str = "login.json";
const NEXT: &str = "login.new";
const LIMIT: u64 = 64 * 1024;
static WRITER: Mutex<()> = Mutex::new(());

#[derive(Deserialize, Serialize)]
pub struct SavedLogin {
    pub token: Option<String>,
    pub refresh: Option<String>,
    pub expires: Option<u64>,
}

pub fn directory() -> PathBuf {
    crate::config::dir().join("session")
}

fn private_dir(path: &Path, create: bool) -> io::Result<File> {
    if create {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match DirBuilder::new().mode(0o700).create(path) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => (),
        }
    }
    let dir = open(path, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?;
    let info = fstat(&dir)?;
    if info.st_uid != geteuid().as_raw() || info.st_mode & 0o077 != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "права папки сеанса"));
    }
    Ok(dir.into())
}

pub fn exists(path: &Path) -> bool {
    match private_dir(path, false) {
        Ok(dir) => !matches!(rustix::fs::statat(dir, SESSION, AtFlags::SYMLINK_NOFOLLOW), Err(rustix::io::Errno::NOENT)),
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}

pub fn load(path: &Path) -> io::Result<SavedLogin> {
    let dir = private_dir(path, false)?;
    let fd = openat(dir, SESSION, OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC, Mode::empty())?;
    let info = fstat(&fd)?;
    if FileType::from_raw_mode(info.st_mode) != FileType::RegularFile || info.st_uid != geteuid().as_raw()
        || info.st_mode & 0o077 != 0 || info.st_nlink != 1
    {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "права файла сеанса"));
    }
    let mut data = Vec::new();
    File::from(fd).take(LIMIT + 1).read_to_end(&mut data)?;
    if data.len() as u64 > LIMIT {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "размер файла сеанса"));
    }
    serde_json::from_slice(&data).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "формат файла сеанса"))
}

fn remove_item(dir: &File, name: &str) -> io::Result<()> {
    match unlinkat(dir, name, AtFlags::empty()) {
        Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub fn save(path: &Path, session: &SavedLogin) -> io::Result<()> {
    let _writer = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let dir = private_dir(path, true)?;
    remove_item(&dir, NEXT)?;
    let fd = openat(&dir, NEXT, OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR)?;
    let mut next = File::from(fd);
    let result = (|| {
        serde_json::to_writer(&mut next, session)?;
        next.flush()?;
        next.sync_all()?;
        renameat(&dir, NEXT, &dir, SESSION)?;
        dir.sync_all()
    })();
    if result.is_err() {
        remove_item(&dir, NEXT)?;
    }
    result
}

pub fn remove(path: &Path) -> io::Result<()> {
    let _writer = WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let dir = match private_dir(path, false) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        result => result?,
    };
    remove_item(&dir, NEXT)?;
    remove_item(&dir, SESSION)?;
    dir.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn private_session_survives_restart_refresh_and_logout() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session");
        assert!(!exists(&path));
        let mut login = SavedLogin { token: Some("test-access".into()), refresh: Some("test-refresh".into()), expires: Some(20) };
        save(&path, &login).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(path.join(SESSION)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load(&path).unwrap().refresh.as_deref(), Some("test-refresh"));
        login.token = Some("updated".into());
        save(&path, &login).unwrap();
        assert_eq!(load(&path).unwrap().token, login.token);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        remove(&path).unwrap();
        remove(&path).unwrap();
        assert!(!exists(&path));
    }

    #[test]
    fn rejects_exposed_files_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session");
        let login = SavedLogin { token: Some("fixture".into()), refresh: None, expires: None };
        save(&path, &login).unwrap();
        let file = path.join(SESSION);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path).is_err());
        let other = temp.path().join("other");
        std::fs::rename(&file, &other).unwrap();
        symlink(&other, &file).unwrap();
        assert!(load(&path).is_err());
        remove(&path).unwrap();
        assert!(other.exists());
    }
}
