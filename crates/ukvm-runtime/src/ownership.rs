//! Linux user-runtime ownership and a read-only, advisory status snapshot.
//! The permanent lock inode is never removed. Kernel ownership, not PID files,
//! decides whether a runtime exists; all snapshots are ignored without that lock.
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub use crate::snapshot::{PeerStatus, RuntimeSnapshot, RuntimeStatus};

pub fn directory() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .ok_or("XDG_RUNTIME_DIR is missing; run UKVM in your logged-in graphical user session")?;
    let base = PathBuf::from(base);
    if !base.is_absolute() {
        return Err("XDG_RUNTIME_DIR must be absolute".into());
    }
    private_directory(&base, false)?;
    let dir = base.join("ukvm");
    private_directory(&dir, true)?;
    Ok(dir)
}
fn private_directory(dir: &Path, create: bool) -> Result<(), String> {
    if create {
        match fs::create_dir(dir) {
            Ok(()) => fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let metadata = fs::symlink_metadata(dir)
        .map_err(|e| format!("runtime directory {}: {e}", dir.display()))?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "runtime directory {} must be a private directory owned by this user (mode 0700)",
            dir.display()
        ));
    }
    Ok(())
}
fn lock_file(dir: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(dir.join("runtime.lock"))
        .map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err("runtime lock must be a private regular file owned by this user".into());
    }
    Ok(file)
}

pub struct RuntimeOwner {
    _lock: File,
    dir: PathBuf,
}
impl RuntimeOwner {
    pub fn acquire() -> Result<Self, String> {
        Self::acquire_at(&directory()?)
    }
    fn acquire_at(dir: &Path) -> Result<Self, String> {
        private_directory(dir, false)?;
        let lock = lock_file(dir)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                "UKVM runtime already owns this user session; use ukvm status or ukvm service stop before running another instance".into()
            } else { format!("cannot acquire runtime lock: {e}") }
        })?;
        // A crash may leave an advisory snapshot behind. It grants no authority.
        let _ = fs::remove_file(dir.join("status.toml"));
        Ok(Self {
            _lock: lock,
            dir: dir.to_owned(),
        })
    }
    pub fn publish(&self, mut snapshot: RuntimeSnapshot) -> Result<(), String> {
        snapshot.pid = std::process::id();
        snapshot.updated = now();
        let text = toml::to_string(&snapshot).map_err(|e| e.to_string())?;
        if text.len() > 64 * 1024 {
            return Err("runtime snapshot exceeds size limit".into());
        }
        let pending = self.dir.join("status.pending");
        // Replace a crash's pending file, never follow a symlink.
        let _ = fs::remove_file(&pending);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending)
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        fs::rename(pending, self.dir.join("status.toml")).map_err(|e| e.to_string())
    }
}
impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.dir.join("status.toml"));
        let _ = fs::remove_file(self.dir.join("status.pending"));
        // Dropping the file releases flock, including on unwinding. Do not unlink it.
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn status() -> Result<RuntimeStatus, String> {
    status_at(&directory()?)
}
fn status_at(dir: &Path) -> Result<RuntimeStatus, String> {
    let lock = lock_file(dir)?;
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => return Ok(RuntimeStatus::Stopped),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(format!("cannot inspect runtime ownership: {e}")),
    }
    let read = || -> Result<RuntimeSnapshot, String> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(dir.join("status.toml"))
            .map_err(|e| e.to_string())?;
        let mut text = String::new();
        file.take(64 * 1024 + 1)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        if text.len() > 64 * 1024 {
            return Err("snapshot exceeds size limit".into());
        }
        let snapshot: RuntimeSnapshot = toml::from_str(&text).map_err(|e| e.to_string())?;
        if snapshot.updated > now().saturating_add(1) || now().saturating_sub(snapshot.updated) > 5
        {
            return Err(
                "runtime owns the lock but its snapshot is stale; focus/availability unknown"
                    .into(),
            );
        }
        Ok(snapshot)
    };
    Ok(match read() {
        Ok(snapshot) => RuntimeStatus::Running(Box::new(snapshot)),
        Err(error) => RuntimeStatus::StartingOrUnavailable(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn private_temp() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    #[test]
    fn exclusive_ownership_clean_shutdown_and_stale_snapshot_recovery() {
        let dir = private_temp();
        let owner = RuntimeOwner::acquire_at(dir.path()).unwrap();
        assert!(RuntimeOwner::acquire_at(dir.path()).is_err());
        assert!(matches!(
            status_at(dir.path()).unwrap(),
            RuntimeStatus::StartingOrUnavailable(_)
        ));
        owner
            .publish(RuntimeSnapshot {
                focus: "Local".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(
            status_at(dir.path()).unwrap(),
            RuntimeStatus::Running(_)
        ));
        drop(owner);
        assert!(dir.path().join("runtime.lock").exists());
        assert!(!dir.path().join("status.toml").exists());
        fs::write(dir.path().join("status.toml"), "stale garbage").unwrap();
        assert!(matches!(
            status_at(dir.path()).unwrap(),
            RuntimeStatus::Stopped
        ));
        let owner = RuntimeOwner::acquire_at(dir.path()).unwrap();
        assert!(!dir.path().join("status.toml").exists());
        drop(owner);
    }
    #[test]
    fn stale_and_malformed_live_snapshots_do_not_claim_focus() {
        let dir = private_temp();
        let owner = RuntimeOwner::acquire_at(dir.path()).unwrap();
        let stale = RuntimeSnapshot {
            updated: 1,
            focus: "Controlling".into(),
            ..Default::default()
        };
        fs::write(
            dir.path().join("status.toml"),
            toml::to_string(&stale).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            status_at(dir.path()).unwrap(),
            RuntimeStatus::StartingOrUnavailable(_)
        ));
        fs::write(dir.path().join("status.toml"), "bad snapshot").unwrap();
        assert!(matches!(
            status_at(dir.path()).unwrap(),
            RuntimeStatus::StartingOrUnavailable(_)
        ));
        owner.publish(Default::default()).unwrap();
        assert!(matches!(
            status_at(dir.path()).unwrap(),
            RuntimeStatus::Running(_)
        ));
    }
    #[test]
    fn symlink_lock_and_public_directory_fail_closed() {
        let dir = private_temp();
        let target = dir.path().join("other");
        fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("runtime.lock")).unwrap();
        assert!(RuntimeOwner::acquire_at(dir.path()).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "untouched");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(RuntimeOwner::acquire_at(dir.path()).is_err());
    }
}
