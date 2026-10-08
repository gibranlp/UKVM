//! Resolve one current graphical session before starting a systemd user runtime.
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::{Path, PathBuf},
    time::Duration,
};

pub const SESSION_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DBUS_SESSION_BUS_ADDRESS",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "DISPLAY",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];
pub type ServiceEnvironment = BTreeMap<String, String>;
const PORTAL_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DBUS_SESSION_BUS_ADDRESS",
    "HYPRLAND_INSTANCE_SIGNATURE",
];

async fn bus(runtime: &Path) -> Result<zbus::Connection, String> {
    let address = format!("unix:path={}/bus", runtime.display());
    zbus::connection::Builder::address(address.as_str())
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| format!("user session bus is not ready: {e}"))
}

fn runtime_directory() -> Result<PathBuf, String> {
    let path = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!("/run/user/{}", rustix::process::getuid().as_raw()))
        });
    let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if !path.is_absolute()
        || !metadata.is_dir()
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err("XDG_RUNTIME_DIR must be a private directory owned by this user".into());
    }
    Ok(path)
}

fn process_started(pid: u32) -> Result<u64, String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|e| e.to_string())?;
    stat.rsplit_once(')')
        .and_then(|(_, tail)| tail.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| "cannot determine compositor process start time".into())
}

fn connect(path: &Path) -> Result<UnixStream, String> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    let fd = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|e| e.to_string())?;
    let address = SocketAddrUnix::new(path).map_err(|e| e.to_string())?;
    rustix::net::connect(&fd, &address).map_err(|e| format!("Wayland socket unavailable: {e}"))?;
    Ok(fd.into())
}

fn socket_peer(runtime: &Path, display: &str, since: u64) -> Result<u32, String> {
    if display.is_empty() {
        return Err("WAYLAND_DISPLAY is empty".into());
    }
    let path = runtime.join(display);
    let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err("Wayland socket must belong to this user and not be a symlink".into());
    }
    let stream = connect(&path)?;
    let peer = rustix::net::sockopt::socket_peercred(&stream).map_err(|e| e.to_string())?;
    if peer.uid != rustix::process::getuid() {
        return Err("Wayland compositor belongs to a different user".into());
    }
    let pid = peer.pid.as_raw_nonzero().get() as u32;
    // /proc usa ticks; redondear el límite igual evita rechazos por resolución del reloj.
    let ticks = since.saturating_mul(rustix::param::clock_ticks_per_second()) / 1_000_000;
    if process_started(pid)? < ticks {
        return Err("Wayland compositor predates the current graphical login".into());
    }
    Ok(pid)
}

fn hyprland_sessions(runtime: &Path, since: u64) -> Result<Vec<(String, String)>, String> {
    let directory = runtime.join("hypr");
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut sessions = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= 64 {
            return Err("too many Hyprland runtime instances; refusing ambiguous session".into());
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let signature = entry.file_name().to_string_lossy().into_owned();
        if !signature
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            continue;
        }
        let lock = entry.path().join("hyprland.lock");
        let Ok(metadata) = fs::symlink_metadata(&lock) else {
            continue;
        };
        if !metadata.is_file() || metadata.uid() != rustix::process::getuid().as_raw() {
            continue;
        }
        let mut text = String::new();
        let Ok(file) = fs::File::open(lock) else {
            continue;
        };
        if file.take(4096).read_to_string(&mut text).is_err() {
            continue;
        }
        let mut lines = text.lines();
        let Some(pid) = lines.next().and_then(|line| line.parse::<u32>().ok()) else {
            continue;
        };
        let Some(display) = lines.next() else {
            continue;
        };
        if socket_peer(runtime, display, since).ok() != Some(pid) {
            continue;
        }
        let Ok(control) = connect(&entry.path().join(".socket.sock")) else {
            continue;
        };
        let Ok(peer) = rustix::net::sockopt::socket_peercred(control) else {
            continue;
        };
        if peer.pid.as_raw_nonzero().get() as u32 == pid && peer.uid == rustix::process::getuid() {
            sessions.push((signature, display.to_owned()));
        }
    }
    Ok(sessions)
}

fn resolve(
    runtime: &Path,
    mut env: ServiceEnvironment,
    since: u64,
) -> Result<ServiceEnvironment, String> {
    let sessions = hyprland_sessions(runtime, since)?;
    match sessions.as_slice() {
        [(signature, display)] => {
            env.insert("WAYLAND_DISPLAY".into(), display.clone());
            env.insert("HYPRLAND_INSTANCE_SIGNATURE".into(), signature.clone());
            env.insert("XDG_CURRENT_DESKTOP".into(), "Hyprland".into());
            env.insert("XDG_SESSION_DESKTOP".into(), "Hyprland".into());
            env.remove("DISPLAY");
        }
        [] => {
            let display = env
                .get("WAYLAND_DISPLAY")
                .ok_or("graphical session has not published WAYLAND_DISPLAY yet")?;
            socket_peer(runtime, display, since)?;
            if env
                .get("XDG_CURRENT_DESKTOP")
                .is_some_and(|v| v.split(':').any(|v| v.eq_ignore_ascii_case("hyprland")))
            {
                return Err("current Hyprland runtime instance is not ready".into());
            }
            env.remove("HYPRLAND_INSTANCE_SIGNATURE");
        }
        _ => return Err(
            "multiple current Hyprland instances; simultaneous graphical sessions are unsupported"
                .into(),
        ),
    }
    env.retain(|key, _| SESSION_ENV.contains(&key.as_str()));
    env.insert("XDG_RUNTIME_DIR".into(), runtime.display().to_string());
    env.insert(
        "DBUS_SESSION_BUS_ADDRESS".into(),
        format!("unix:path={}/bus", runtime.display()),
    );
    env.insert("XDG_SESSION_TYPE".into(), "wayland".into());
    Ok(env)
}

/// No input capture, portal grant, identity creation or service action occurs here.
async fn login_since() -> Result<u64, String> {
    let connection = zbus::Connection::system()
        .await
        .map_err(|e| format!("login manager is not ready: {e}"))?;
    let manager = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await
    .map_err(|e| e.to_string())?;
    type Session = (String, u32, String, String, zbus::zvariant::OwnedObjectPath);
    let sessions: Vec<Session> = manager
        .call("ListSessions", &())
        .await
        .map_err(|e| e.to_string())?;
    let mut current = Vec::new();
    for (_, uid, _, _, path) in sessions {
        if uid != rustix::process::getuid().as_raw() {
            continue;
        }
        let session = zbus::Proxy::new(
            &connection,
            "org.freedesktop.login1",
            path,
            "org.freedesktop.login1.Session",
        )
        .await
        .map_err(|e| e.to_string())?;
        let active: bool = session
            .get_property("Active")
            .await
            .map_err(|e| e.to_string())?;
        let kind: String = session
            .get_property("Type")
            .await
            .map_err(|e| e.to_string())?;
        if active && kind == "wayland" {
            current.push(
                session
                    .get_property::<u64>("TimestampMonotonic")
                    .await
                    .map_err(|e| e.to_string())?,
            );
        }
    }
    match current.as_slice() {
        [since] if *since != 0 => Ok(*since),
        [] => Err("no active Wayland login is ready".into()),
        _ => Err("multiple active Wayland logins are unsupported".into()),
    }
}

async fn environment_at(runtime: &Path, login_since: u64) -> Result<ServiceEnvironment, String> {
    let connection = bus(runtime).await?;
    let manager = zbus::Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
    )
    .await
    .map_err(|e| e.to_string())?;
    let path: zbus::zvariant::OwnedObjectPath = manager
        .call("GetUnit", &("graphical-session.target",))
        .await
        .map_err(|e| e.to_string())?;
    let unit = zbus::Proxy::new(
        &connection,
        "org.freedesktop.systemd1",
        path,
        "org.freedesktop.systemd1.Unit",
    )
    .await
    .map_err(|e| e.to_string())?;
    let state: String = unit
        .get_property("ActiveState")
        .await
        .map_err(|e| e.to_string())?;
    if !matches!(state.as_str(), "active" | "activating") {
        return Err("graphical-session.target is not starting/active".into());
    }
    let values: Vec<String> = manager
        .get_property("Environment")
        .await
        .map_err(|e| e.to_string())?;
    let env = values
        .iter()
        .filter_map(|v| v.split_once('='))
        .filter(|(key, _)| SESSION_ENV.contains(key))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    resolve(runtime, env, login_since)
}

pub async fn service_environment() -> Result<ServiceEnvironment, String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let runtime = runtime_directory()?;
        environment_at(&runtime, login_since().await?).await
    })
    .await
    .map_err(|_| "graphical session readiness query timed out".to_string())?
}

/// Publish only validated session variables before D-Bus activates a portal backend.
pub async fn service_portal_ready(env: &ServiceEnvironment) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let connection = bus(Path::new(&env["XDG_RUNTIME_DIR"])).await?;
        let manager = zbus::Proxy::new(
            &connection,
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
        )
        .await
        .map_err(|e| e.to_string())?;
        let missing: Vec<_> = PORTAL_ENV
            .iter()
            .filter(|key| !env.contains_key(**key))
            .copied()
            .collect();
        manager
            .call::<_, _, ()>("UnsetEnvironment", &(missing,))
            .await
            .map_err(|e| e.to_string())?;
        let values: Vec<_> = env
            .iter()
            .filter(|(key, _)| PORTAL_ENV.contains(&key.as_str()))
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        manager
            .call::<_, _, ()>("SetEnvironment", &(values,))
            .await
            .map_err(|e| e.to_string())?;
        // D-Bus no permite borrar variables: vaciar las ausentes evita conservar otra sesión.
        let activation: std::collections::HashMap<_, _> = PORTAL_ENV
            .iter()
            .map(|key| (*key, env.get(*key).map(String::as_str).unwrap_or("")))
            .collect();
        let daemon = zbus::Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await
        .map_err(|e| e.to_string())?;
        daemon
            .call::<_, _, ()>("UpdateActivationEnvironment", &(activation,))
            .await
            .map_err(|e| e.to_string())?;
        let portal = zbus::Proxy::new(
            &connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.DBus.Peer",
        )
        .await
        .map_err(|e| e.to_string())?;
        portal
            .call::<_, _, ()>("Ping", &())
            .await
            .map_err(|e| format!("portal frontend is not ready: {e}"))?;
        for interface in [
            "org.freedesktop.portal.InputCapture",
            "org.freedesktop.portal.RemoteDesktop",
        ] {
            let proxy = zbus::Proxy::new(
                &connection,
                "org.freedesktop.portal.Desktop",
                "/org/freedesktop/portal/desktop",
                interface,
            )
            .await
            .map_err(|e| e.to_string())?;
            if proxy.get_property::<u32>("version").await.is_ok() {
                return Ok(());
            }
        }
        Err("no input portal backend is ready yet".into())
    })
    .await
    .map_err(|_| "portal readiness query timed out".to_string())?
}
