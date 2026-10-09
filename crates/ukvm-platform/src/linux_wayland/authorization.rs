//! Credenciales privadas de un solo uso del portal, independientes de la confianza entre pares.
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_RECORD: u64 = 16_384;

#[derive(Clone, Copy)]
pub enum Kind {
    RemoteDesktop,
    InputCapture,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::RemoteDesktop => "remote-desktop",
            Self::InputCapture => "input-capture",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context {
    desktop: String,
    version: u32,
}
impl Context {
    #[cfg(test)]
    pub fn current(version: u32) -> Self {
        Self {
            desktop: std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .to_lowercase(),
            version,
        }
    }

    pub async fn on_bus(bus: &zbus::Connection, version: u32) -> Result<Self, String> {
        let daemon = zbus::Proxy::new(
            bus,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        .map_err(|_| "cannot identify the active desktop portal")?;
        let owner: String = daemon
            .call("GetNameOwner", &("org.freedesktop.portal.Desktop",))
            .await
            .map_err(|_| "desktop portal is not running")?;
        let pid: u32 = daemon
            .call("GetConnectionUnixProcessID", &(&owner,))
            .await
            .map_err(|_| "cannot identify desktop portal process")?;
        let uid: u32 = daemon
            .call("GetConnectionUnixUser", &(&owner,))
            .await
            .map_err(|_| "cannot verify desktop portal owner")?;
        if uid != rustix::process::getuid().as_raw() {
            return Err("desktop portal must belong to the current user".into());
        }
        let mut bytes = Vec::new();
        File::open(format!("/proc/{pid}/environ"))
            .and_then(|file| file.take(1_048_577).read_to_end(&mut bytes))
            .map_err(|_| "cannot inspect desktop portal context")?;
        if bytes.len() > 1_048_576 {
            return Err("desktop portal context exceeds size limit".into());
        }
        let desktop = portal_desktop(&bytes)?;
        let current_owner: String = daemon
            .call("GetNameOwner", &("org.freedesktop.portal.Desktop",))
            .await
            .map_err(|_| "desktop portal disappeared during context validation")?;
        if current_owner != owner {
            return Err("desktop portal changed during context validation; retry".into());
        }
        Ok(Self { desktop, version })
    }
}

// Se usa el entorno del portal que eligió el backend, nunca el de la terminal o del servicio cliente.
fn portal_desktop(bytes: &[u8]) -> Result<String, String> {
    let desktop = bytes
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"XDG_CURRENT_DESKTOP="))
        .and_then(|value| std::str::from_utf8(value).ok())
        .ok_or("desktop portal has no valid desktop identity")?;
    let names: Vec<String> = desktop
        .split(':')
        .map(|name| name.trim().to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    if names.is_empty() || names.iter().any(|name| name.chars().any(char::is_control)) {
        return Err("desktop portal has no valid desktop identity".into());
    }
    Ok(names.join(":"))
}

fn observed_context(version: u32) -> Option<Context> {
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(3), async {
                    let bus = zbus::Connection::session().await.ok()?;
                    Context::on_bus(&bus, version).await.ok()
                })
                .await
                .ok()
                .flatten()
            })
    })
    .join()
    .ok()
    .flatten()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Missing,
    Corrupt,
    BackendChanged,
    Unsupported,
    RestorePending,
    NeedsApproval,
    Remembered,
    BackendDidNotPersist,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    format_version: u32,
    context: Context,
    token: Option<String>,
    authorized: bool,
    reason: Reason,
}

#[derive(Debug, Serialize)]
pub struct Status {
    pub state: &'static str,
    pub active: bool,
    pub remembered: bool,
    pub persistence_api_supported: Option<bool>,
    pub reason: Reason,
    pub context_verified: bool,
}

pub fn directory() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(".local/state")))
        .ok_or("cannot locate private portal authorization state")?;
    if !base.is_absolute() {
        return Err("XDG_STATE_HOME must be absolute".into());
    }
    Ok(base.join("ukvm/portal-authorization"))
}

fn private_directory(dir: &Path, create: bool) -> Result<bool, String> {
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|_| "cannot create private portal authorization directory")?;
    }
    for path in [dir.parent().expect("authorization parent"), dir] {
        let metadata = match fs::symlink_metadata(path) {
            Ok(value) => value,
            Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => return Err("cannot inspect portal authorization directory".into()),
        };
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err("portal authorization directory must be user-owned, private and not a symlink (0700)".into());
        }
    }
    Ok(true)
}

fn private_file(path: &Path, create: bool) -> Result<Option<File>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)
    {
        Ok(value) => value,
        Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(
                "cannot open private portal authorization file; symlinks are refused".into(),
            )
        }
    };
    let metadata = file
        .metadata()
        .map_err(|_| "cannot inspect portal authorization file")?;
    if !metadata.is_file()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(
            "portal authorization file must be private, regular and user-owned (0600)".into(),
        );
    }
    Ok(Some(file))
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && !token.chars().any(char::is_control)
}

fn read(dir: &Path, kind: Kind) -> Result<(Option<Record>, Reason), String> {
    let Some(file) = private_file(&dir.join(format!("{}.json", kind.name())), false)? else {
        return Ok((None, Reason::Missing));
    };
    let mut bytes = Vec::new();
    file.take(MAX_RECORD + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read portal authorization state")?;
    if bytes.len() as u64 > MAX_RECORD {
        return Ok((None, Reason::Corrupt));
    }
    match serde_json::from_slice::<Record>(&bytes) {
        Ok(record)
            if record.format_version == 1 && record.token.as_deref().is_none_or(valid_token) =>
        {
            Ok((Some(record), Reason::Missing))
        }
        _ => Ok((None, Reason::Corrupt)),
    }
}

fn write(dir: &Path, kind: Kind, record: &Record) -> Result<(), String> {
    let mut pending = tempfile::NamedTempFile::new_in(dir)
        .map_err(|_| "cannot prepare private portal authorization replacement")?;
    serde_json::to_writer(&mut pending, record)
        .map_err(|_| "cannot encode portal authorization state")?;
    pending
        .flush()
        .map_err(|_| "cannot flush portal authorization state")?;
    pending
        .as_file()
        .sync_all()
        .map_err(|_| "cannot synchronize portal authorization state")?;
    pending
        .persist(dir.join(format!("{}.json", kind.name())))
        .map_err(|_| "cannot replace portal authorization state")?;
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|_| "cannot synchronize portal authorization directory".into())
}

// El candado conserva su inode y dura toda la sesión, incluso mientras se espera el consentimiento.
pub struct Authorization {
    dir: PathBuf,
    kind: Kind,
    record: Record,
    restore_token: Option<String>,
    _lock: File,
}
impl Authorization {
    pub fn begin(kind: Kind, context: Context) -> Result<Self, String> {
        Self::begin_in(&directory()?, kind, context)
    }
    pub(super) fn begin_in(dir: &Path, kind: Kind, context: Context) -> Result<Self, String> {
        private_directory(dir, true)?;
        let lock = private_file(&dir.join(format!("{}.lock", kind.name())), true)?.unwrap();
        lock.try_lock_exclusive().map_err(|_| {
            "desktop authorization is already in use; close the existing session first"
        })?;
        let (previous, mut reason) = read(dir, kind)?;
        let restore_token = match previous {
            Some(record) if record.context != context => {
                reason = Reason::BackendChanged;
                None
            }
            Some(record) => record.token,
            None => None,
        };
        let restore_token = if context.version < 2 {
            reason = Reason::Unsupported;
            None
        } else {
            if restore_token.is_some() {
                reason = Reason::RestorePending;
            }
            restore_token
        };
        let record = Record {
            format_version: 1,
            context,
            token: None,
            authorized: false,
            reason,
        };
        // Se retira el token antes de enviarlo: una cancelación o caída no debe reutilizarlo.
        write(dir, kind, &record)?;
        Ok(Self {
            dir: dir.to_owned(),
            kind,
            record,
            restore_token,
            _lock: lock,
        })
    }
    pub fn restore_token(&self) -> Option<&str> {
        self.restore_token.as_deref()
    }
    pub fn granted(&mut self, token: Option<&str>) -> Result<(), String> {
        if token.is_some_and(|value| !valid_token(value)) {
            return Err("portal returned invalid persistent authorization state".into());
        }
        if token.is_some() && token == self.restore_token.as_deref() {
            return Err("portal did not rotate its single-use authorization credential".into());
        }
        self.record.token = token.map(str::to_owned);
        self.record.authorized = true;
        self.record.reason = if token.is_some() {
            Reason::Remembered
        } else if self.record.context.version < 2 {
            Reason::Unsupported
        } else {
            Reason::BackendDidNotPersist
        };
        // La rotación queda durable antes de permitir el uso de la sesión autorizada.
        write(&self.dir, self.kind, &self.record)
    }
}
impl Drop for Authorization {
    fn drop(&mut self) {
        self.record.authorized = false;
        if self.record.reason == Reason::RestorePending {
            self.record.reason = Reason::NeedsApproval;
        }
        let _ = write(&self.dir, self.kind, &self.record);
        let _ = FileExt::unlock(&self._lock);
    }
}

pub fn status(kind: Kind, version: Option<u32>) -> Result<Status, String> {
    status_in(&directory()?, kind, version)
}
fn status_in(dir: &Path, kind: Kind, version: Option<u32>) -> Result<Status, String> {
    let supported = version.map(|v| v >= 2);
    let missing = |reason| Status {
        state: if supported == Some(false) {
            "unsupported"
        } else {
            "needs_approval"
        },
        active: false,
        remembered: false,
        persistence_api_supported: supported,
        reason,
        context_verified: false,
    };
    if !private_directory(dir, false)? {
        return Ok(missing(Reason::Missing));
    }
    let (record, reason) = read(dir, kind)?;
    let Some(record) = record else {
        return Ok(missing(reason));
    };
    let context = observed_context(version.unwrap_or(record.context.version));
    if context
        .as_ref()
        .is_some_and(|context| record.context != *context)
        || version.is_some_and(|version| record.context.version != version)
    {
        return Ok(missing(Reason::BackendChanged));
    }
    if supported == Some(false) {
        return Ok(missing(Reason::Unsupported));
    }
    let busy = if let Some(lock) = private_file(&dir.join(format!("{}.lock", kind.name())), false)?
    {
        match lock.try_lock_exclusive() {
            Ok(()) => false,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(_) => return Err("cannot inspect desktop authorization ownership".into()),
        }
    } else {
        false
    };
    let active = busy && record.authorized;
    let remembered = record.token.is_some();
    Ok(Status {
        state: if remembered {
            "persistent"
        } else if active {
            "active"
        } else if record.reason == Reason::Unsupported {
            "unsupported"
        } else {
            "needs_approval"
        },
        active,
        remembered,
        persistence_api_supported: supported.or(Some(record.context.version >= 2)),
        reason: record.reason,
        context_verified: context.is_some(),
    })
}

pub fn reset() -> Result<(), String> {
    reset_in(&directory()?)
}
fn reset_in(dir: &Path) -> Result<(), String> {
    if !private_directory(dir, false)? {
        return Ok(());
    }
    let mut locks = Vec::new();
    for kind in [Kind::RemoteDesktop, Kind::InputCapture] {
        let lock = private_file(&dir.join(format!("{}.lock", kind.name())), true)?.unwrap();
        lock.try_lock_exclusive().map_err(|_| "desktop authorization is still in use; stop UKVM and any diagnostic portal sessions first")?;
        private_file(&dir.join(format!("{}.json", kind.name())), false)?;
        locks.push(lock);
    }
    for kind in [Kind::RemoteDesktop, Kind::InputCapture] {
        match fs::remove_file(dir.join(format!("{}.json", kind.name()))) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(
                    "cannot forget portal authorization; retry reset after checking private state"
                        .into(),
                )
            }
        }
    }
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|_| "cannot synchronize authorization reset".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ukvm/portal-authorization");
        (tmp, dir)
    }
    fn context() -> Context {
        Context::current(2)
    }
    #[test]
    fn first_approval_rotation_and_restart_do_not_leak_secrets() {
        let (_tmp, dir) = fixture();
        let mut first = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        assert!(first.restore_token().is_none());
        first.granted(Some("secret-first")).unwrap();
        let status = status_in(&dir, Kind::RemoteDesktop, Some(2)).unwrap();
        assert!(status.active && status.remembered);
        assert!(!format!("{status:?}").contains("secret-first"));
        assert!(!serde_json::to_string(&status)
            .unwrap()
            .contains("secret-first"));
        drop(first);
        let mut next = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        assert_eq!(next.restore_token(), Some("secret-first"));
        assert!(!fs::read_to_string(dir.join("remote-desktop.json"))
            .unwrap()
            .contains("secret-first"));
        next.granted(Some("secret-replacement")).unwrap();
        drop(next);
        let next = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        assert_eq!(next.restore_token(), Some("secret-replacement"));
        assert_eq!(fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(dir.join("remote-desktop.json"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }
    #[test]
    fn rejection_or_crash_consumes_old_token_and_allows_normal_consent() {
        let (_tmp, dir) = fixture();
        let mut first = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        first.granted(Some("expired")).unwrap();
        drop(first);
        let restored = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        assert_eq!(restored.restore_token(), Some("expired"));
        drop(restored);
        let mut fresh = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        assert!(fresh.restore_token().is_none());
        fresh.granted(None).unwrap();
        let state = status_in(&dir, Kind::RemoteDesktop, Some(2)).unwrap();
        assert_eq!(state.state, "active");
        assert_eq!(state.reason, Reason::BackendDidNotPersist);
    }
    #[test]
    fn missing_corrupt_changed_backend_and_unsupported_capture() {
        let (_tmp, dir) = fixture();
        assert_eq!(
            status_in(&dir, Kind::RemoteDesktop, Some(2)).unwrap().state,
            "needs_approval"
        );
        let mut first = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        first.granted(Some("old-backend")).unwrap();
        drop(first);
        let mut changed = context();
        changed.desktop = "a-different-desktop".into();
        let attempt = Authorization::begin_in(&dir, Kind::RemoteDesktop, changed).unwrap();
        assert!(attempt.restore_token().is_none());
        assert_eq!(attempt.record.reason, Reason::BackendChanged);
        drop(attempt);
        fs::write(dir.join("remote-desktop.json"), "broken secret data").unwrap();
        assert_eq!(
            status_in(&dir, Kind::RemoteDesktop, Some(2))
                .unwrap()
                .reason,
            Reason::Corrupt
        );
        let attempt = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        assert!(attempt.restore_token().is_none());
        drop(attempt);
        let attempt =
            Authorization::begin_in(&dir, Kind::InputCapture, Context::current(1)).unwrap();
        assert_eq!(attempt.record.reason, Reason::Unsupported);
    }
    #[test]
    fn reset_closes_authority_boundary_without_changing_trust() {
        let (_tmp, dir) = fixture();
        let mut first = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        first.granted(Some("remembered")).unwrap();
        assert!(reset_in(&dir).is_err());
        assert!(Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).is_err());
        drop(first);
        fs::write(
            dir.parent().unwrap().join("trust-independent"),
            "trusted-peer",
        )
        .unwrap();
        reset_in(&dir).unwrap();
        assert!(!dir.join("remote-desktop.json").exists());
        assert!(dir.join("remote-desktop.lock").exists());
        assert_eq!(
            fs::read_to_string(dir.parent().unwrap().join("trust-independent")).unwrap(),
            "trusted-peer"
        );
    }
    #[test]
    fn insecure_files_and_symlinks_fail_closed() {
        let (_tmp, dir) = fixture();
        let first = Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).unwrap();
        drop(first);
        fs::set_permissions(
            dir.join("remote-desktop.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).is_err());
        assert!(reset_in(&dir).is_err());
        fs::remove_file(dir.join("remote-desktop.json")).unwrap();
        std::os::unix::fs::symlink("remote-desktop.lock", dir.join("remote-desktop.json")).unwrap();
        assert!(Authorization::begin_in(&dir, Kind::RemoteDesktop, context()).is_err());
    }
    #[test]
    fn interface_version_changes_and_invalid_rotation_require_fresh_approval() {
        let (_tmp, dir) = fixture();
        let mut first = Authorization::begin_in(&dir, Kind::InputCapture, context()).unwrap();
        first.granted(Some("single-use-secret")).unwrap();
        drop(first);
        let mut same = Authorization::begin_in(&dir, Kind::InputCapture, context()).unwrap();
        assert!(same.granted(Some("single-use-secret")).is_err());
        assert!(same.granted(Some("bad\nsecret")).is_err());
        drop(same);
        let mut first = Authorization::begin_in(&dir, Kind::InputCapture, context()).unwrap();
        first.granted(Some("new-secret")).unwrap();
        drop(first);
        let attempt =
            Authorization::begin_in(&dir, Kind::InputCapture, Context::current(3)).unwrap();
        assert!(attempt.restore_token().is_none());
        assert_eq!(attempt.record.reason, Reason::BackendChanged);
    }

    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    #[test]
    fn portal_context_process_fixture() {
        let Ok(role) = std::env::var("UKVM_CONTEXT_TEST_ROLE") else {
            return;
        };
        if role == "status" {
            println!(
                "STATUS:{}",
                serde_json::to_string(&status(Kind::RemoteDesktop, None).unwrap()).unwrap()
            );
            return;
        }
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let _portal = zbus::connection::Builder::session()
                    .unwrap()
                    .name("org.freedesktop.portal.Desktop")
                    .unwrap()
                    .build()
                    .await
                    .unwrap();
                println!("READY");
                std::io::stdout().flush().unwrap();
                tokio::task::spawn_blocking(|| {
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).unwrap();
                })
                .await
                .unwrap();
            });
    }
    fn portal_process(address: &str, desktop: &str) -> Process {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "linux_wayland::authorization::tests::portal_context_process_fixture",
                "--nocapture",
            ])
            .env("UKVM_CONTEXT_TEST_ROLE", "portal")
            .env("DBUS_SESSION_BUS_ADDRESS", address)
            .env("XDG_CURRENT_DESKTOP", desktop)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line.contains("READY") {
                break;
            }
            line.clear();
        }
        Process(child)
    }
    #[test]
    fn same_session_service_approval_is_visible_from_different_cli_environments() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let (tmp, dir) = fixture();
        let mut bus = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _bus = Process(bus);
        let address = address.trim();
        let portal = portal_process(address, "Hyprland");
        let context_on_bus = || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let bus = zbus::connection::Builder::address(address)
                        .unwrap()
                        .build()
                        .await
                        .unwrap();
                    Context::on_bus(&bus, 2).await.unwrap()
                })
        };
        let context = context_on_bus();
        assert_eq!(context.desktop, "hyprland");
        let query = |desktop: Option<&str>| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "linux_wayland::authorization::tests::portal_context_process_fixture",
                    "--nocapture",
                ])
                .env("UKVM_CONTEXT_TEST_ROLE", "status")
                .env("DBUS_SESSION_BUS_ADDRESS", address)
                .env("XDG_STATE_HOME", tmp.path());
            if let Some(desktop) = desktop {
                command.env("XDG_CURRENT_DESKTOP", desktop);
            } else {
                command.env_remove("XDG_CURRENT_DESKTOP");
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(!text.contains("private-remembered-token"));
            serde_json::from_str::<serde_json::Value>(
                text.lines()
                    .find_map(|line| line.strip_prefix("STATUS:"))
                    .unwrap(),
            )
            .unwrap()
        };
        let mut approved =
            Authorization::begin_in(&dir, Kind::RemoteDesktop, context.clone()).unwrap();
        approved.granted(Some("private-remembered-token")).unwrap();
        for desktop in [None, Some(""), Some("SpectrumOS:Hyprland"), Some("KDE")] {
            let status = query(desktop);
            assert_eq!(status["state"], "persistent");
            assert_eq!(status["remembered"], true);
            assert_eq!(status["active"], true);
            assert_eq!(status["context_verified"], true);
        }
        drop(approved);
        drop(portal);
        let portal = portal_process(address, "Hyprland");
        assert!(context_on_bus() == context);
        let restored =
            Authorization::begin_in(&dir, Kind::RemoteDesktop, context_on_bus()).unwrap();
        assert_eq!(restored.restore_token(), Some("private-remembered-token"));
        drop(restored);
        drop(portal);
        let _portal = portal_process(address, "KDE");
        assert_eq!(query(Some("Hyprland"))["reason"], "backend_changed");
        let changed = Authorization::begin_in(&dir, Kind::RemoteDesktop, context_on_bus()).unwrap();
        assert!(changed.restore_token().is_none());
    }
    #[test]
    fn portal_desktop_identity_excludes_volatile_and_unrelated_environment() {
        let first = portal_desktop(b"XDG_CURRENT_DESKTOP=Hyprland\0WAYLAND_DISPLAY=wayland-1\0HYPRLAND_INSTANCE_SIGNATURE=old\0").unwrap();
        let reboot = portal_desktop(b"WAYLAND_DISPLAY=wayland-8\0XDG_CURRENT_DESKTOP=HYPRLAND\0HYPRLAND_INSTANCE_SIGNATURE=new\0").unwrap();
        assert_eq!(first, reboot);
        assert_ne!(first, portal_desktop(b"XDG_CURRENT_DESKTOP=KDE\0").unwrap());
        assert!(portal_desktop(b"XDG_CURRENT_DESKTOP=\0").is_err());
        assert!(portal_desktop(b"WAYLAND_DISPLAY=wayland-1\0").is_err());
    }
}
