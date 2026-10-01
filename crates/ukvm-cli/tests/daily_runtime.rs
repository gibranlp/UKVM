#![cfg(target_os = "linux")]
use std::{fs, os::unix::fs::PermissionsExt, process::Command};
struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        // Deliberately isolated systemctl shim: no host service state is changed.
        let shim = dir.path().join("bin/systemctl");
        fs::write(&shim, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$UKVM_TEST_SYSTEMCTL_LOG\"\ncase \"$*\" in\n*show*) printf 'LoadState=not-found\\nActiveState=inactive\\nMainPID=0\\n';;\n*is-active*) [ \"$UKVM_TEST_GRAPHICAL_ACTIVE\" = 1 ] || exit 1;;\nesac\nexit 0\n").unwrap();
        fs::set_permissions(shim, fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ukvm"));
        command
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/ukvm-test-bus",
            )
            .env(
                "UKVM_TEST_SYSTEMCTL_LOG",
                self.dir.path().join("systemctl.log"),
            )
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.path().join("bin").display()),
            );
        command
    }
    fn log(&self) -> String {
        fs::read_to_string(self.dir.path().join("systemctl.log")).unwrap_or_default()
    }
}
#[test]
fn service_install_enable_stop_disable_uninstall_are_explicit_and_preserve_identity() {
    let fixture = Fixture::new();
    assert!(fixture
        .command()
        .args(["service", "install", "--verbose"])
        .output()
        .unwrap()
        .status
        .success());
    let unit = fixture.dir.path().join("config/systemd/user/ukvm.service");
    assert!(fs::read_to_string(&unit).unwrap().contains("run --verbose"));
    assert_eq!(fixture.log(), "--user daemon-reload\n");
    for action in ["enable", "stop", "disable", "status"] {
        assert!(fixture
            .command()
            .args(["service", action])
            .output()
            .unwrap()
            .status
            .success());
    }
    assert!(!fixture.log().contains(" start "));
    let data = fixture.dir.path().join("data/ukvm");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("node.key"), "retain identity").unwrap();
    assert!(fixture
        .command()
        .args(["service", "uninstall"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!unit.exists());
    assert!(fixture.log().contains("--user disable --now ukvm.service"));
    assert_eq!(
        fs::read_to_string(data.join("node.key")).unwrap(),
        "retain identity"
    );
    assert!(!fixture
        .command()
        .args(["service", "invented"])
        .output()
        .unwrap()
        .status
        .success());
}
#[test]
fn offline_status_and_peers_do_not_create_identity_or_claim_readiness() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.dir.path().join("config/ukvm")).unwrap();
    fs::write(
        fixture.dir.path().join("config/ukvm/config.toml"),
        "[node]\nname='offline-test'\n",
    )
    .unwrap();
    let output = fixture.command().arg("status").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    for expected in [
        "offline-test",
        "not initialized",
        "Runtime: stopped",
        "ActiveState=inactive",
        "Configured topology",
        "unknown",
        "No trusted peers",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    let peers = fixture.command().arg("peers").output().unwrap();
    assert!(peers.status.success());
    assert!(String::from_utf8(peers.stdout)
        .unwrap()
        .contains("No trusted peers"));
    assert!(!fixture.dir.path().join("data/ukvm/node.key").exists());
    assert!(!fixture.log().contains(" start "));
}
#[test]
fn orphan_snapshot_is_ignored_without_kernel_ownership() {
    let fixture = Fixture::new();
    // Isolate ownership tests without mutating environment in this test process.
    let private = fixture.dir.path().join("ukvm");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let lock = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(private.join("runtime.lock"))
        .unwrap();
    fs::set_permissions(
        private.join("runtime.lock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    // Snapshot without a kernel owner must always be ignored, including forged focus.
    fs::write(private.join("status.toml"), "focus='Controlling'\n").unwrap();
    let output = fixture.command().arg("status").output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Runtime: stopped"));
    assert!(!text.contains("Focus: Controlling"));
    drop(lock);
}
#[test]
fn start_without_native_session_does_not_start_or_import_environment() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["service", "start"])
        .env("WAYLAND_DISPLAY", "/nonexistent/wayland-test")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("native Wayland"));
    assert!(fixture.log().is_empty());
}

#[test]
fn start_requires_active_target_then_imports_only_allowlisted_session_names() {
    use std::os::unix::net::UnixListener;
    let fixture = Fixture::new();
    let socket = fixture.dir.path().join("wayland");
    let _listener = UnixListener::bind(&socket).unwrap();
    let inactive = fixture
        .command()
        .args(["service", "start"])
        .env("WAYLAND_DISPLAY", &socket)
        .output()
        .unwrap();
    assert!(!inactive.status.success());
    assert!(String::from_utf8(inactive.stderr)
        .unwrap()
        .contains("graphical-session.target is inactive"));
    assert!(!fixture.log().contains("import-environment"));
    for action in ["start", "restart"] {
        let output = fixture
            .command()
            .args(["service", action])
            .env("WAYLAND_DISPLAY", &socket)
            .env("UKVM_TEST_GRAPHICAL_ACTIVE", "1")
            .env("SECRET_NOT_TO_IMPORT", "value")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let log = fixture.log();
    assert!(log.contains("--user import-environment WAYLAND_DISPLAY"));
    assert!(log.contains("--user unset-environment"));
    assert!(log.contains("--user start ukvm.service"));
    assert!(log.contains("--user restart ukvm.service"));
    assert!(!log.contains("SECRET_NOT_TO_IMPORT"));
    assert!(!log.contains(&socket.to_string_lossy().to_string()));
}

#[test]
fn killed_runtime_leaves_no_kernel_owner_and_restarts_with_existing_identity() {
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Stdio},
        time::{Duration, Instant},
    };
    struct RuntimeChild(Child, Option<std::thread::JoinHandle<()>>);
    impl Drop for RuntimeChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
            if let Some(reader) = self.1.take() {
                reader.join().unwrap();
            }
        }
    }
    let fixture = Fixture::new();
    let config = fixture.dir.path().join("config/ukvm");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "[node]\nname='crash-test'\n[network]\nlisten='127.0.0.1:0'\n",
    )
    .unwrap();
    let start = || {
        let mut child = RuntimeChild(
            fixture
                .command()
                .arg("run")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            None,
        );
        let stdout = child.0.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        child.1 = Some(std::thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(stdout);
            reader.read_line(&mut line).unwrap();
            let _ = tx.send(line);
            let mut rest = String::new();
            std::io::Read::read_to_string(&mut reader, &mut rest).unwrap();
        }));
        let line = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(line.starts_with("UKVM running as crash-test"), "{line}");
        child
    };
    let mut first = start();
    let snapshot = fixture.dir.path().join("ukvm/status.toml");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !snapshot.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let certificate = fs::read(fixture.dir.path().join("data/ukvm/node.der")).unwrap();
    first.0.kill().unwrap();
    first.0.wait().unwrap();
    assert!(snapshot.exists()); // SIGKILL cannot execute Drop; state really is stale.
    let output = fixture.command().arg("status").output().unwrap();
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("Runtime: stopped"));
    let mut second = start();
    assert_eq!(
        fs::read(fixture.dir.path().join("data/ukvm/node.der")).unwrap(),
        certificate
    );
    assert!(Command::new("kill")
        .args(["-TERM", &second.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = second.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!snapshot.exists());
}
