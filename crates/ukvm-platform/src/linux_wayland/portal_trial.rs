//! Development-only lifecycle checks for the pinned Hyprland portal trial.
use super::{service_environment, ServiceEnvironment, SESSION_ENV};
use std::{os::unix::process::CommandExt, process::Command, time::Duration};

async fn environment() -> Result<ServiceEnvironment, String> {
    let env = service_environment().await?;
    if env.get("XDG_CURRENT_DESKTOP").map(String::as_str) != Some("Hyprland")
        || !env.contains_key("HYPRLAND_INSTANCE_SIGNATURE")
    {
        return Err("the portal trial requires one current native Hyprland instance".into());
    }
    Ok(env)
}

fn verify_process_environment(bytes: &[u8], env: &ServiceEnvironment) -> Result<(), String> {
    for key in [
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "HYPRLAND_INSTANCE_SIGNATURE",
    ] {
        let expected = format!(
            "{key}={}",
            env.get(key).ok_or("incomplete trial environment")?
        );
        if !bytes
            .split(|b| *b == 0)
            .any(|entry| entry == expected.as_bytes())
        {
            return Err(format!("portal process has stale or missing {key}"));
        }
    }
    Ok(())
}

async fn connection(env: &ServiceEnvironment) -> Result<zbus::Connection, String> {
    zbus::connection::Builder::address(env["DBUS_SESSION_BUS_ADDRESS"].as_str())
        .map_err(|e| e.to_string())?
        .build()
        .await
        .map_err(|e| e.to_string())
}

async fn interfaces(
    connection: &zbus::Connection,
    owner: &str,
    backend: bool,
) -> Result<(), String> {
    let prefix = if backend {
        "org.freedesktop.impl.portal"
    } else {
        "org.freedesktop.portal"
    };
    for (interface, property, min_version) in [
        ("InputCapture", "SupportedCapabilities", 1),
        ("RemoteDesktop", "AvailableDeviceTypes", 2),
    ] {
        let name = format!("{prefix}.{interface}");
        let proxy = zbus::Proxy::new(connection, owner, "/org/freedesktop/portal/desktop", name)
            .await
            .map_err(|e| e.to_string())?;
        let version: u32 = proxy
            .get_property("version")
            .await
            .map_err(|e| format!("{interface} version unavailable: {e}"))?;
        let devices: u32 = proxy
            .get_property(property)
            .await
            .map_err(|e| format!("{interface} capabilities unavailable: {e}"))?;
        if version < min_version || devices & 3 != 3 {
            return Err(format!(
                "{interface} incomplete: version={version}, keyboard/pointer mask={devices}"
            ));
        }
    }
    Ok(())
}

async fn ready(env: &ServiceEnvironment, backend: bool) -> Result<(), String> {
    let connection = connection(env).await?;
    let daemon = zbus::fdo::DBusProxy::new(&connection)
        .await
        .map_err(|e| e.to_string())?;
    let name = if backend {
        "org.freedesktop.impl.portal.desktop.hyprland"
    } else {
        "org.freedesktop.portal.Desktop"
    };
    // Consultar al dueño único evita que status active portales por accidente.
    let owner = daemon
        .get_name_owner(
            name.try_into()
                .map_err(|e: zbus::names::Error| e.to_string())?,
        )
        .await
        .map_err(|e| format!("{name} has no ready owner: {e}"))?;
    let pid = daemon
        .get_connection_unix_process_id(owner.as_ref().into())
        .await
        .map_err(|e| e.to_string())?;
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).map_err(|e| e.to_string())?;
    verify_process_environment(&bytes, env)?;
    interfaces(&connection, owner.as_str(), backend).await
}

pub async fn check(backend: bool) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        ready(&environment().await?, backend).await
    })
    .await
    .map_err(|_| "trial portal readiness query timed out".to_string())?
}

pub async fn launch(binary: &str, backend: bool) -> Result<(), String> {
    if !std::path::Path::new(binary).is_absolute() {
        return Err("trial portal executable must be absolute".into());
    }
    let env = environment().await?;
    if !backend {
        tokio::time::timeout(Duration::from_secs(5), ready(&env, true))
            .await
            .map_err(|_| "trial backend readiness query timed out".to_string())??;
    }
    let mut command = Command::new(binary);
    for key in SESSION_ENV {
        command.env_remove(key);
    }
    // libwayland prefiere este FD heredado sobre WAYLAND_DISPLAY; no reutilizarlo.
    command.env_remove("WAYLAND_SOCKET").envs(env);
    Err(format!("cannot execute trial portal: {}", command.exec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_portal_environment_is_rejected_even_when_display_name_is_reused() {
        let env = ServiceEnvironment::from([
            ("WAYLAND_DISPLAY".into(), "wayland-1".into()),
            ("XDG_RUNTIME_DIR".into(), "/run/user/1000".into()),
            (
                "DBUS_SESSION_BUS_ADDRESS".into(),
                "unix:path=/run/user/1000/bus".into(),
            ),
            ("HYPRLAND_INSTANCE_SIGNATURE".into(), "new_login".into()),
        ]);
        let current = env
            .iter()
            .map(|(k, v)| format!("{k}={v}\0"))
            .collect::<String>();
        assert!(verify_process_environment(current.as_bytes(), &env).is_ok());
        let stale = current.replace("new_login", "old_login");
        assert!(verify_process_environment(stale.as_bytes(), &env)
            .unwrap_err()
            .contains("HYPRLAND_INSTANCE_SIGNATURE"));
        let stale_bus = current.replace("unix:path=/run/user/1000/bus", "unix:path=/old/bus");
        assert!(verify_process_environment(stale_bus.as_bytes(), &env).is_err());
    }

    struct Capture(u32);
    #[zbus::interface(name = "org.freedesktop.impl.portal.InputCapture")]
    impl Capture {
        #[zbus(property, name = "version")]
        fn version(&self) -> u32 {
            1
        }
        #[zbus(property)]
        fn supported_capabilities(&self) -> u32 {
            self.0
        }
    }
    struct Remote(u32);
    #[zbus::interface(name = "org.freedesktop.impl.portal.RemoteDesktop")]
    impl Remote {
        #[zbus(property, name = "version")]
        fn version(&self) -> u32 {
            2
        }
        #[zbus(property)]
        fn available_device_types(&self) -> u32 {
            self.0
        }
    }

    #[tokio::test]
    async fn backend_name_alone_does_not_mean_input_interfaces_are_ready() {
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("bus.conf");
        std::fs::write(&config, format!("<busconfig><type>session</type><listen>unix:path={}/bus</listen><auth>EXTERNAL</auth><policy context=\"default\"><allow send_destination=\"*\"/><allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>", dir.path().display())).unwrap();
        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut bus = Bus(Command::new("dbus-daemon")
            .args([
                "--nofork",
                "--print-address=1",
                &format!("--config-file={}", config.display()),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap());
        use std::io::BufRead;
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let connection = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .serve_at("/org/freedesktop/portal/desktop", Capture(3))
            .unwrap()
            .build()
            .await
            .unwrap();
        let owner = connection.unique_name().unwrap().as_str();
        assert!(interfaces(&connection, owner, true).await.is_err());
        connection
            .object_server()
            .at("/org/freedesktop/portal/desktop", Remote(0))
            .await
            .unwrap();
        assert!(interfaces(&connection, owner, true)
            .await
            .unwrap_err()
            .contains("incomplete"));
        connection
            .object_server()
            .remove::<Remote, _>("/org/freedesktop/portal/desktop")
            .await
            .unwrap();
        connection
            .object_server()
            .at("/org/freedesktop/portal/desktop", Remote(3))
            .await
            .unwrap();
        assert!(interfaces(&connection, owner, true).await.is_ok());
    }
}
