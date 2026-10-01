//! Linux user-service installation/presentation. No input or trust policy here.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
const TEMPLATE: &str = include_str!("../../../assets/systemd/ukvm.service.in");
const MARKER: &str = "# UKVM managed user service (M15)\n";
const SESSION_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DBUS_SESSION_BUS_ADDRESS",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
];

pub fn unit(binary: &Path, verbose: bool) -> Result<String, String> {
    if !binary.is_absolute() {
        return Err("service executable must be an absolute path".into());
    }
    let path = binary
        .to_str()
        .ok_or("service executable path must be UTF-8")?;
    if path.chars().any(char::is_control) {
        return Err("service executable path contains control characters".into());
    }
    // systemd command-line quoting, not shell quoting. Escape specifiers and
    // variable expansion is disabled by the unit's ':' executable prefix;
    // no user-controlled text becomes unit directives.
    let quoted = format!(
        "\"{}\"",
        path.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    );
    Ok(TEMPLATE
        .replace("@UKVM_OPTIONS@", if verbose { " --verbose" } else { "" })
        .replace("@UKVM_EXEC@", &quoted))
}
fn path() -> Result<PathBuf, String> {
    let config = ukvm_config::default_path().map_err(|e| e.to_string())?;
    Ok(config
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot determine config directory")?
        .join("systemd/user/ukvm.service"))
}
fn managed(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if !metadata.is_file() => {
            Err("refusing to replace/remove a non-regular service file".into())
        }
        Ok(_) => {
            let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
            if !text.starts_with(MARKER) {
                return Err(format!(
                    "{} is not a UKVM-managed unit; edit it manually",
                    path.display()
                ));
            }
            Ok(true)
        }
    }
}
fn write_unit(path: &Path, text: &str) -> Result<(), String> {
    managed(path)?;
    fs::create_dir_all(path.parent().ok_or("unit path lacks parent")?)
        .map_err(|e| e.to_string())?;
    let pending = path.with_extension(format!("pending-{}", std::process::id()));
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|e| e.to_string())?;
    let result = file
        .write_all(text.as_bytes())
        .and_then(|()| fs::rename(&pending, path));
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result.map_err(|e| e.to_string())
}
fn systemctl(args: &[&str]) -> Result<(), String> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .map_err(|e| format!("systemctl --user: {e}"))?;
    if !status.success()
        && !(args.first() == Some(&"status") && matches!(status.code(), Some(3 | 4)))
    {
        return Err(format!(
            "systemctl --user {} failed ({status}); inspect journalctl --user -u ukvm",
            args.join(" ")
        ));
    }
    Ok(())
}
fn prepare_session() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if !ukvm_platform::linux_wayland::SessionInfo::detect().wayland_socket_reachable {
        return Err(
            "start/restart must be invoked inside the native Wayland graphical user session".into(),
        );
    }
    let target = Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", "graphical-session.target"])
        .status()
        .map_err(|e| e.to_string())?;
    if !target.success() {
        return Err("graphical-session.target is inactive; configure your desktop's systemd graphical-session integration (see docs/SERVICE.md); UKVM will not start a graphical target itself".into());
    }
    let present: Vec<_> = SESSION_ENV
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_some())
        .collect();
    let missing: Vec<_> = SESSION_ENV
        .iter()
        .copied()
        .filter(|name| std::env::var_os(name).is_none())
        .collect();
    if !missing.is_empty() {
        let mut args = vec!["unset-environment"];
        args.extend(missing);
        systemctl(&args)?;
    }
    if !present.is_empty() {
        let mut args = vec!["import-environment"];
        args.extend(present);
        systemctl(&args)?;
    }
    Ok(())
}
pub fn command(args: &[String]) -> ExitCode {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        return super::fail("service management currently requires Linux with systemd --user");
    }
    #[cfg(target_os = "linux")]
    {
        let result = (|| -> Result<(), String> {
            match args {
                [action] if action == "check-session" => {
                    if !ukvm_platform::linux_wayland::SessionInfo::detect().wayland_socket_reachable {
                        return Err("no reachable native Wayland graphical session; service startup skipped".into());
                    }
                }
                [action] if action == "--help" || action == "help" => println!("Usage: ukvm service install [--verbose]|enable|start|stop|restart|status|disable|uninstall\nInstall does not enable or start. Start/restart require the graphical session."),
                [action, rest @ ..] if action == "install" => {
                    let verbose = match rest { [] => false, [flag] if flag == "--verbose" => true, _ => return Err("usage: ukvm service install [--verbose]".into()) };
                    let path = path()?;
                    write_unit(&path, &unit(&std::env::current_exe().map_err(|e| e.to_string())?, verbose)?)?;
                    systemctl(&["daemon-reload"])?;
                    println!("Installed {}. Enable/start were not requested; existing enabled/running state is unchanged.", path.display());
                }
                [action] if matches!(action.as_str(), "enable" | "disable" | "stop" | "start" | "restart" | "status") => {
                    if action == "start" || action == "restart" { prepare_session()?; }
                    systemctl(&[action, "ukvm.service"])?;
                }
                [action] if action == "uninstall" => {
                    let path = path()?;
                    if !managed(&path)? { println!("No UKVM-managed user unit at {}. Packaged units are removed by their package/install owner.", path.display()); return Ok(()); }
                    systemctl(&["disable", "--now", "ukvm.service"])?;
                    fs::remove_file(&path).map_err(|e| e.to_string())?;
                    systemctl(&["daemon-reload"])?;
                    println!("Removed {}. Binary, identity, configuration and trust are retained; a packaged vendor unit may still exist.", path.display());
                }
                _ => return Err("usage: ukvm service install [--verbose]|enable|start|stop|restart|status|disable|uninstall".into()),
            }
            Ok(())
        })();
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => super::fail(&error),
        }
    }
}
/// Read-only manager state for `ukvm status`; a missing bus is reported, not fatal.
pub fn summary() -> String {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut child = match Command::new("systemctl")
        .args([
            "--user",
            "show",
            "ukvm.service",
            "--property=LoadState,ActiveState,SubState,UnitFileState,MainPID",
            "--no-pager",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return format!("unavailable: {error}"),
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return "unavailable (user manager query timed out/failed)".into();
            }
        }
    }
    match child.wait_with_output() {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .trim()
            .replace('\n', ", "),
        _ => "unavailable (no loaded user unit or user manager)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_service_is_user_graphical_explicit_and_safely_quoted() {
        let text = unit(Path::new("/tmp/a b%$\\\"/ukvm"), true).unwrap();
        assert!(text.contains("ExecStart=:\"/tmp/a b%%$\\\\\\\"/ukvm\" run --verbose"));
        for directive in [
            "PartOf=graphical-session.target",
            "After=graphical-session-pre.target",
            "ExecCondition=",
            "WantedBy=graphical-session.target",
            "Restart=on-failure",
            "KillSignal=SIGINT",
            "ConditionUser=!root",
        ] {
            assert!(text.contains(directive));
        }
        assert!(!text.contains("User=root"));
        assert!(!text.contains("default.target"));
        assert!(unit(Path::new("relative"), false).is_err());
        assert!(unit(Path::new("/tmp/bad\nExecStart=x"), false).is_err());
    }
    #[test]
    fn installation_preserves_unmanaged_unit_and_refuses_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ukvm.service");
        fs::write(&path, "custom unit").unwrap();
        assert!(write_unit(&path, &unit(Path::new("/bin/ukvm"), false).unwrap()).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "custom unit");
        fs::remove_file(&path).unwrap();
        let text = unit(Path::new("/bin/ukvm"), false).unwrap();
        write_unit(&path, &text).unwrap();
        write_unit(&path, &unit(Path::new("/bin/ukvm"), true).unwrap()).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("--verbose"));
        #[cfg(unix)]
        {
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(dir.path().join("target"), &path).unwrap();
            assert!(write_unit(&path, &text).is_err());
        }
    }
}
