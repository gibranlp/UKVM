//! Private per-user Windows runtime ownership. A permanent kernel-locked file
//! decides ownership; bounded snapshots are advisory, never input authority.
pub use crate::snapshot::{PeerStatus, RuntimeSnapshot, RuntimeStatus};
use std::{
    ffi::OsStr,
    fs::File,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use ukvm_platform::windows::{session, state::StateDirectory};

pub fn directory() -> Result<PathBuf, String> {
    ukvm_platform::windows::state::runtime_directory().map_err(|e| e.to_string())
}
fn contended(error: &std::io::Error) -> bool {
    error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct RuntimeOwner {
    _lock: File,
    directory: StateDirectory,
    session_id: Option<u32>,
}
impl RuntimeOwner {
    pub fn acquire() -> Result<Self, String> {
        // Require a normal current interactive desktop before creating runtime
        // state. This probe neither grants peer trust nor captures/injects input.
        let session = session::probe().map_err(|e| e.to_string())?;
        session.require_interactive().map_err(|e| e.to_string())?;
        let mut owner = Self::acquire_at(&directory()?)?;
        owner.session_id = Some(session.session_id);
        Ok(owner)
    }
    fn acquire_at(path: &Path) -> Result<Self, String> {
        let directory = StateDirectory::open(path, true).map_err(|e| e.to_string())?;
        let lock = directory
            .lock_file(OsStr::new("runtime.lock"), true)
            .map_err(|e| e.to_string())?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|e| {
            if contended(&e) {
                "UKVM runtime already owns this user session".into()
            } else {
                format!("cannot acquire runtime lock: {e}")
            }
        })?;
        match directory.remove(OsStr::new("status.toml")) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        Ok(Self {
            _lock: lock,
            directory,
            session_id: None,
        })
    }
    pub fn session_id(&self) -> Option<u32> {
        self.session_id
    }
    pub fn publish(&self, mut snapshot: RuntimeSnapshot) -> Result<(), String> {
        snapshot.pid = std::process::id();
        snapshot.updated = now();
        let text = toml::to_string(&snapshot).map_err(|e| e.to_string())?;
        if text.len() > 64 * 1024 {
            return Err("runtime snapshot exceeds size limit".into());
        }
        self.directory
            .replace(OsStr::new("status.toml"), text.as_bytes())
            .map_err(|e| e.to_string())
    }
}
impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        let _ = self.directory.remove(OsStr::new("status.toml"));
    }
}

pub fn status() -> Result<RuntimeStatus, String> {
    status_at(&directory()?)
}
fn status_at(path: &Path) -> Result<RuntimeStatus, String> {
    let directory = match StateDirectory::open(path, false) {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RuntimeStatus::Stopped),
        Err(e) => return Err(e.to_string()),
    };
    let lock = match directory.lock_file(OsStr::new("runtime.lock"), false) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(RuntimeStatus::Stopped),
        Err(e) => return Err(e.to_string()),
    };
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => return Ok(RuntimeStatus::Stopped),
        Err(e) if contended(&e) => {}
        Err(e) => return Err(format!("cannot inspect runtime ownership: {e}")),
    }
    let read = || -> Result<RuntimeSnapshot, String> {
        let bytes = directory
            .read(OsStr::new("status.toml"), 64 * 1024)
            .map_err(|e| e.to_string())?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "invalid runtime snapshot UTF-8")?;
        let snapshot: RuntimeSnapshot =
            toml::from_str(text).map_err(|_| "invalid runtime snapshot")?;
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

    #[test]
    fn ownership_child_process_fixture() {
        use std::io::{Read, Write};
        let Some(path) = std::env::var_os("UKVM_OWNERSHIP_TEST_DIRECTORY") else {
            return;
        };
        let owner = RuntimeOwner::acquire_at(Path::new(&path)).unwrap();
        owner.publish(RuntimeSnapshot::default()).unwrap();
        println!("ownership-ready");
        std::io::stdout().flush().unwrap();
        let mut command = [0];
        let _ = std::io::stdin().read_exact(&mut command);
    }

    #[test]
    fn abrupt_process_exit_releases_kernel_lock_and_stale_snapshot_grants_nothing() {
        use std::{
            io::{BufRead, BufReader},
            process::{Child, Command, Stdio},
        };
        struct Actor(Child);
        impl Drop for Actor {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime");
        let mut actor = Actor(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "ownership::tests::ownership_child_process_fixture",
                    "--nocapture",
                ])
                .env("UKVM_OWNERSHIP_TEST_DIRECTORY", &path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut output = BufReader::new(actor.0.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "ownership fixture exited before readiness"
            );
            if line.trim_end().ends_with("ownership-ready") {
                break;
            }
        }
        assert!(RuntimeOwner::acquire_at(&path).is_err());
        actor.0.kill().unwrap();
        actor.0.wait().unwrap();
        assert!(path.join("runtime.lock").exists());
        assert!(matches!(status_at(&path).unwrap(), RuntimeStatus::Stopped));
        assert!(RuntimeOwner::acquire_at(&path).is_ok());
    }
    #[test]
    fn exclusive_ownership_snapshot_bounds_and_permanent_lock_lifetime() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime");
        assert!(matches!(status_at(&path).unwrap(), RuntimeStatus::Stopped));
        assert!(!path.exists());
        let owner = RuntimeOwner::acquire_at(&path).unwrap();
        assert!(RuntimeOwner::acquire_at(&path).is_err());
        assert!(std::fs::remove_file(path.join("runtime.lock")).is_err());
        assert!(matches!(
            status_at(&path).unwrap(),
            RuntimeStatus::StartingOrUnavailable(_)
        ));
        owner
            .publish(RuntimeSnapshot {
                state: "running".into(),
                focus: "Local".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(matches!(
            status_at(&path).unwrap(),
            RuntimeStatus::Running(_)
        ));
        assert!(owner
            .publish(RuntimeSnapshot {
                name: "x".repeat(65536),
                ..Default::default()
            })
            .is_err());
        drop(owner);
        assert!(path.join("runtime.lock").exists());
        assert!(matches!(status_at(&path).unwrap(), RuntimeStatus::Stopped));
        let owner = RuntimeOwner::acquire_at(&path).unwrap();
        owner
            .directory
            .replace(OsStr::new("status.toml"), b"not a valid snapshot")
            .unwrap();
        assert!(matches!(
            status_at(&path).unwrap(),
            RuntimeStatus::StartingOrUnavailable(_)
        ));
        let text = toml::to_string(&RuntimeSnapshot {
            updated: now().saturating_sub(10),
            ..Default::default()
        })
        .unwrap();
        owner
            .directory
            .replace(OsStr::new("status.toml"), text.as_bytes())
            .unwrap();
        assert!(matches!(
            status_at(&path).unwrap(),
            RuntimeStatus::StartingOrUnavailable(_)
        ));
    }
    #[test]
    fn insecure_lock_and_snapshot_hardlinks_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime");
        let owner = RuntimeOwner::acquire_at(&path).unwrap();
        owner.publish(RuntimeSnapshot::default()).unwrap();
        std::fs::hard_link(path.join("status.toml"), temp.path().join("snapshot-link")).unwrap();
        assert!(matches!(
            status_at(&path).unwrap(),
            RuntimeStatus::StartingOrUnavailable(_)
        ));
        drop(owner);
        // Remove the test hard link so the next owner may safely clear advisory state.
        std::fs::remove_file(temp.path().join("snapshot-link")).unwrap();
        std::fs::hard_link(path.join("runtime.lock"), temp.path().join("lock-link")).unwrap();
        assert!(RuntimeOwner::acquire_at(&path).is_err());
        assert!(status_at(&path).is_err());
    }
    #[test]
    fn lock_is_released_when_an_owner_unwinds() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("runtime");
        let result = std::panic::catch_unwind(|| {
            let _owner = RuntimeOwner::acquire_at(&path).unwrap();
            panic!("owned fixture unwind");
        });
        assert!(result.is_err());
        assert!(RuntimeOwner::acquire_at(&path).is_ok());
    }
}
