#![cfg(target_os = "linux")]
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use zbus::zvariant::OwnedObjectPath;

#[derive(Default)]
struct State {
    environment: Vec<String>,
    active: bool,
    since: u64,
}
struct Manager(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl Manager {
    fn get_unit(&self, _name: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/graphical").unwrap()
    }
    #[zbus(property)]
    fn environment(&self) -> Vec<String> {
        self.0.lock().unwrap().environment.clone()
    }
    fn set_environment(&self, values: Vec<String>) {
        let mut state = self.0.lock().unwrap();
        for value in values {
            let name = value.split_once('=').unwrap().0;
            state
                .environment
                .retain(|v| v.split_once('=').unwrap().0 != name);
            state.environment.push(value);
        }
    }
    fn unset_environment(&self, names: Vec<String>) {
        self.0
            .lock()
            .unwrap()
            .environment
            .retain(|v| !names.iter().any(|name| v.starts_with(&format!("{name}="))));
    }
}
struct Unit(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.systemd1.Unit")]
impl Unit {
    #[zbus(property)]
    fn active_state(&self) -> &str {
        if self.0.lock().unwrap().active {
            "active"
        } else {
            "inactive"
        }
    }
}
struct Login;
#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Login {
    fn list_sessions(&self) -> Vec<(String, u32, String, String, OwnedObjectPath)> {
        use std::os::unix::fs::MetadataExt;
        vec![(
            "test".into(),
            fs::metadata("/proc/self").unwrap().uid(),
            "test".into(),
            "seat0".into(),
            OwnedObjectPath::try_from("/login").unwrap(),
        )]
    }
}
struct Session(Arc<Mutex<State>>);
#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl Session {
    #[zbus(property)]
    fn active(&self) -> bool {
        self.0.lock().unwrap().active
    }
    #[zbus(property)]
    fn r#type(&self) -> &str {
        "wayland"
    }
    #[zbus(property)]
    fn timestamp_monotonic(&self) -> u64 {
        self.0.lock().unwrap().since
    }
}
struct Portal;
#[zbus::interface(name = "org.freedesktop.portal.InputCapture")]
impl Portal {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    _bus: Process,
    connection: zbus::Connection,
    state: Arc<Mutex<State>>,
    sockets: Vec<UnixListener>,
}
impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let address = format!("unix:path={}/bus", dir.path().display());
        let config = dir.path().join("bus.conf");
        fs::write(&config, format!("<busconfig><type>session</type><listen>{address}</listen><auth>EXTERNAL</auth><policy context=\"default\"><allow send_destination=\"*\"/><allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>")).unwrap();
        let mut daemon = Process(
            Command::new("dbus-daemon")
                .args([
                    "--nofork",
                    "--nopidfile",
                    "--print-address=1",
                    &format!("--config-file={}", config.display()),
                ])
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("DISPLAY")
                .env("XDG_RUNTIME_DIR", dir.path())
                .stdout(Stdio::piped())
                .spawn()
                .expect("dbus-daemon is required for isolated service tests"),
        );
        let mut ready = String::new();
        BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert!(ready.starts_with("unix:path="));
        let state = Arc::new(Mutex::new(State {
            active: true,
            since: 1,
            ..State::default()
        }));
        let connection = zbus::connection::Builder::address(address.as_str())
            .unwrap()
            .name("org.freedesktop.systemd1")
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at("/org/freedesktop/systemd1", Manager(state.clone()))
            .unwrap()
            .serve_at("/graphical", Unit(state.clone()))
            .unwrap()
            .serve_at("/org/freedesktop/login1", Login)
            .unwrap()
            .serve_at("/login", Session(state.clone()))
            .unwrap()
            .build()
            .await
            .unwrap();
        let config = dir.path().join("config/ukvm");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("config.toml"),
            "[node]\nname='login-test'\n[network]\nlisten='127.0.0.1:0'\n",
        )
        .unwrap();
        state.lock().unwrap().environment = vec![
            format!("XDG_CONFIG_HOME={}/config", dir.path().display()),
            format!("XDG_DATA_HOME={}/data", dir.path().display()),
            format!("XDG_STATE_HOME={}/state", dir.path().display()),
            "SECRET_NOT_TO_IMPORT=secret".into(),
            "DBUS_SESSION_BUS_ADDRESS=unix:path=/stale/bus".into(),
        ];
        Self {
            dir,
            _bus: daemon,
            connection,
            state,
            sockets: Vec::new(),
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ukvm"));
        command
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env(
                "DBUS_SYSTEM_BUS_ADDRESS",
                format!("unix:path={}/bus", self.dir.path().display()),
            )
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/stale/inherited-bus")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE");
        command
    }
    async fn output(&self, args: &[&str]) -> std::process::Output {
        let mut command = self.command();
        command.args(args);
        tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap()
    }
    fn socket(&mut self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        self.sockets.push(UnixListener::bind(&path).unwrap());
        path
    }
    fn hyprland(&mut self) {
        self.socket("wayland-current");
        self.socket("hypr/current_1/.socket.sock");
        fs::write(
            self.dir.path().join("hypr/current_1/hyprland.lock"),
            format!("{}\nwayland-current\n", std::process::id()),
        )
        .unwrap();
    }
    async fn portal(&self) {
        self.connection
            .object_server()
            .at("/org/freedesktop/portal/desktop", Portal)
            .await
            .unwrap();
        self.connection
            .request_name("org.freedesktop.portal.Desktop")
            .await
            .unwrap();
    }
    async fn start(&self) -> Process {
        let mut child = Process(
            self.command()
                .args(["service", "run"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(stdout);
            reader.read_line(&mut line).unwrap();
            let _ = send.send(line);
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        let line = tokio::time::timeout(Duration::from_secs(8), receive)
            .await
            .unwrap()
            .unwrap();
        if !line.starts_with("UKVM running as login-test") {
            let mut errors = String::new();
            child
                .0
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut errors)
                .unwrap();
            panic!("{line}: {errors}");
        }
        child
    }
    async fn terminate(&self, mut child: Process) {
        assert!(Command::new("kill")
            .args(["-TERM", &child.0.id().to_string()])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                let mut errors = String::new();
                child
                    .0
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut errors)
                    .unwrap();
                assert!(status.success(), "{status}: {errors}");
                break;
            }
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!self.dir.path().join("ukvm/status.toml").exists());
    }
}

#[tokio::test]
async fn absent_login_environment_then_hyprland_and_portal_ready_retries_without_capture() {
    let mut fixture = Fixture::new().await;
    let missing = fixture.output(&["service", "run"]).await;
    assert_eq!(
        missing.status.code(),
        Some(75),
        "{}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(!fixture.dir.path().join("ukvm/runtime.lock").exists());
    fixture.hyprland();
    let portal_missing = fixture.output(&["service", "run"]).await;
    assert_eq!(portal_missing.status.code(), Some(75));
    assert!(!fixture.dir.path().join("ukvm/runtime.lock").exists());
    fixture.portal().await;
    let child = fixture.start().await;
    let env = fixture.state.lock().unwrap().environment.clone();
    assert!(env.iter().any(|v| v == "WAYLAND_DISPLAY=wayland-current"));
    assert!(env
        .iter()
        .any(|v| v == "HYPRLAND_INSTANCE_SIGNATURE=current_1"));
    assert!(env.iter().any(|v| v
        == &format!(
            "DBUS_SESSION_BUS_ADDRESS=unix:path={}/bus",
            fixture.dir.path().display()
        )));
    let second = fixture.output(&["service", "run"]).await;
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already owns"));
    fixture.terminate(child).await;
    let again = fixture.start().await;
    fixture.terminate(again).await;
    fixture.state.lock().unwrap().active = false;
    let logout = fixture.output(&["service", "run"]).await;
    assert_eq!(logout.status.code(), Some(75));
    assert!(!fixture.dir.path().join("ukvm/status.toml").exists());
}

#[tokio::test]
async fn stale_alive_compositor_and_dead_wayland_socket_are_rejected_for_new_login() {
    let mut fixture = Fixture::new().await;
    fixture.hyprland();
    {
        let mut state = fixture.state.lock().unwrap();
        state.since = u64::MAX / 2;
        state.environment.extend([
            "WAYLAND_DISPLAY=wayland-current".into(),
            "XDG_CURRENT_DESKTOP=GNOME".into(),
        ]);
    }
    let stale = fixture.output(&["service", "check-session"]).await;
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("predates the current graphical login"));
    fixture.state.lock().unwrap().since = 1;
    assert!(fixture
        .output(&["service", "check-session"])
        .await
        .status
        .success());
    fixture.sockets.clear();
    let dead = fixture.output(&["service", "check-session"]).await;
    assert!(!dead.status.success());
    assert!(!fixture.dir.path().join("ukvm/runtime.lock").exists());
}

#[tokio::test]
async fn another_wayland_desktop_rechecks_late_manager_environment_and_cleans_up() {
    let mut fixture = Fixture::new().await;
    assert_eq!(
        fixture.output(&["service", "run"]).await.status.code(),
        Some(75)
    );
    let socket = fixture.socket("wayland-other");
    {
        let mut state = fixture.state.lock().unwrap();
        state.environment.extend([
            format!("WAYLAND_DISPLAY={}", socket.display()),
            "XDG_CURRENT_DESKTOP=GNOME".into(),
            "HYPRLAND_INSTANCE_SIGNATURE=stale_previous_login".into(),
        ]);
    }
    fixture.portal().await;
    let child = fixture.start().await;
    assert!(!fixture
        .state
        .lock()
        .unwrap()
        .environment
        .iter()
        .any(|v| v.starts_with("HYPRLAND_INSTANCE_SIGNATURE=")));
    fixture.terminate(child).await;
}

#[tokio::test]
async fn trial_helper_uses_new_hyprland_session_without_inherited_wayland_environment() {
    let mut fixture = Fixture::new().await;
    let mut command = fixture.command();
    command = {
        let mut helper = Command::new(env!("CARGO_BIN_EXE_ukvm-portal-trial-session"));
        helper.envs(command.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))));
        helper
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE");
        helper
    };
    command
        .args(["backend", "/usr/bin/env"])
        .env("WAYLAND_SOCKET", "999");
    let (mut command, missing) = tokio::task::spawn_blocking(move || {
        let output = command.output().unwrap();
        (command, output)
    })
    .await
    .unwrap();
    assert_eq!(missing.status.code(), Some(75));
    fixture.hyprland();
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let env = String::from_utf8(output.stdout).unwrap();
    assert!(env.lines().any(|v| v == "WAYLAND_DISPLAY=wayland-current"));
    assert!(env
        .lines()
        .any(|v| v == "HYPRLAND_INSTANCE_SIGNATURE=current_1"));
    assert!(!env.lines().any(|v| v.starts_with("WAYLAND_SOCKET=")));
    assert!(!fixture.dir.path().join("ukvm/runtime.lock").exists());
}
