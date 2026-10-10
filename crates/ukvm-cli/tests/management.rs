#![cfg(target_os = "linux")]
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
};
use ukvm_network::{identity::Identity, trust::TrustStore};

struct Profile {
    dir: tempfile::TempDir,
}
impl Profile {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(dir.path().join("bin")).unwrap();
        let shim = dir.path().join("bin/systemctl");
        fs::write(&shim, r#"#!/bin/sh
printf '%s\n' "$*" >> "$UKVM_TEST_LOG"
case "$*" in
  *show*) printf 'LoadState=loaded\nActiveState=inactive\nSubState=dead\nUnitFileState=enabled\nMainPID=0\n';;
esac
exit "${UKVM_TEST_SYSTEMCTL_FAIL:-0}"
"#).unwrap();
        fs::set_permissions(shim, fs::Permissions::from_mode(0o700)).unwrap();
        Self { dir }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ukvm"));
        command
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_DATA_HOME", self.dir.path().join("data"))
            .env("XDG_STATE_HOME", self.dir.path().join("state"))
            .env("XDG_RUNTIME_DIR", self.dir.path())
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/ukvm-management-test-bus",
            )
            .env_remove("WAYLAND_DISPLAY")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.path().join("bin").display()),
            )
            .env("UKVM_TEST_LOG", self.dir.path().join("systemctl.log"));
        command
    }
    fn json(&self, args: &[&str], success: bool) -> Value {
        let output = self.command().args(args).arg("--json").output().unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stdout)));
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], success);
        value
    }
    fn add_peer(&self, alias: &str) -> String {
        let peer =
            Identity::load_or_create(&self.dir.path().join(format!("identity-{alias}"))).unwrap();
        let state = self.dir.path().join("state/ukvm");
        let mut store = TrustStore::load(&state).unwrap();
        store
            .import(
                &state,
                alias.into(),
                Some("127.0.0.1:24800".into()),
                peer.tls_cert().as_ref(),
            )
            .unwrap();
        peer.id
    }
    fn configuration(&self) -> Vec<u8> {
        fs::read(self.dir.path().join("config/ukvm/config.toml")).unwrap()
    }
}

#[test]
fn json_reads_are_versioned_offline_and_do_not_initialize_identity() {
    let profile = Profile::new();
    let status = profile.json(&["status"], true);
    assert!(status["data"]["node"]["node_id"].is_null());
    assert_eq!(status["data"]["runtime"]["state"], "stopped");
    assert_eq!(status["data"]["health"]["identity_initialized"], false);
    assert!(status["data"]["configuration"]["disk_matches_runtime"].is_null());
    assert_eq!(profile.json(&["peers"], true)["data"]["peers"], json!([]));
    assert_eq!(
        profile.json(&["layout", "show"], true)["data"]["nodes"][0]["x"],
        0
    );
    let doctor = profile.json(&["doctor"], true);
    assert_eq!(doctor["data"]["consent_requested"], false);
    assert_eq!(
        doctor["data"]["remote_desktop"]["pointer_advertised"],
        false
    );
    assert!(!profile.dir.path().join("data/ukvm/node.key").exists());
    for args in [
        vec!["status", "oops"],
        vec!["discover", "--interface"],
        vec!["layout", "place", "0", "0", "ghost"],
        vec!["service", "uninstall"],
        vec!["pair", "--yes"],
    ] {
        profile.json(&args, false);
    }
}

#[test]
fn topology_edits_validate_duplicates_trust_adjacency_and_preserve_config_on_error() {
    let profile = Profile::new();
    let io = profile.add_peer("io");
    profile.add_peer("third");
    profile.json(&["layout", "place", "1", "0", "io"], true);
    let before = profile.configuration();
    for args in [
        vec!["layout", "place", "1", "0", "third"],
        vec!["layout", "place", "0", "1", "io"],
        vec!["layout", "place", "1", "1", "third"],
        vec!["layout", "place", "0", "0", "third"],
        vec!["layout", "place", "3", "0", "third"],
        vec!["layout", "place", "0", "2", "third"],
        vec!["layout", "place", "-1", "0", "ghost"],
        vec!["layout", "remove", "io"],
    ] {
        profile.json(&args, false);
        assert_eq!(profile.configuration(), before);
    }
    profile.json(&["layout", "move", "io", "-1", "0"], true);
    let peers = profile.json(&["peers"], true);
    let peer = peers["data"]["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["node_id"] == io)
        .unwrap();
    assert_eq!(peer["position"], json!({"x": -1, "y": 0}));
    assert_eq!(peer["availability"]["state"], "unknown");
    profile.json(&["layout", "place", "-2", "0", "third"], true);
    let before = profile.configuration();
    profile.json(&["layout", "move", "io", "0", "1"], false);
    profile.json(&["layout", "remove", "io", "--yes"], false);
    assert_eq!(profile.configuration(), before);
    profile.json(&["layout", "remove", "third", "--yes"], true);
    profile.json(&["layout", "remove", "io", "--yes"], true);
    profile.json(&["layout", "place", "1", "0", "io"], true);
    profile.json(&["layout", "remove", "right", "--yes"], true);
}

#[test]
fn explicit_unpair_and_permissions_do_not_grant_remote_authority() {
    let profile = Profile::new();
    profile.add_peer("io");
    profile.json(&["layout", "place", "1", "0", "io"], true);
    assert_eq!(
        profile.json(&["peer", "permissions", "io"], true)["data"]["permissions"]
            ["control_this_node"],
        false
    );
    profile.json(&["peer", "allow", "io", "control-this-node"], true);
    assert_eq!(
        profile.json(&["peer", "permissions", "io"], true)["data"]["permissions"]
            ["control_this_node"],
        true
    );
    profile.json(&["peer", "deny", "io", "control-this-node"], true);
    profile.json(&["unpair", "io"], false);
    assert_eq!(
        profile.json(&["peers"], true)["data"]["peers"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    profile.json(&["unpair", "io", "--yes"], true);
    assert_eq!(profile.json(&["peers"], true)["data"]["peers"], json!([]));
    assert_eq!(
        profile.json(&["layout"], true)["data"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn invalid_disk_configuration_and_missing_trust_are_visible() {
    let profile = Profile::new();
    fs::create_dir_all(profile.dir.path().join("config/ukvm")).unwrap();
    let config = profile.dir.path().join("config/ukvm/config.toml");
    fs::write(
        &config,
        format!(
            "[node]\nname='test'\n[[layout.nodes]]\nx=1\ny=0\npeer_id='{}'\n",
            "a".repeat(64)
        ),
    )
    .unwrap();
    let layout = profile.json(&["layout"], true);
    assert_eq!(layout["data"]["valid"], false);
    assert_eq!(layout["data"]["issues"][0]["code"], "untrusted_peer");
    profile.json(&["apply"], false);
    fs::write(&config, "not valid TOML =").unwrap();
    let status = profile.json(&["status"], false);
    assert!(status["data"].is_null());
    assert!(status["error"]["message"]
        .as_str()
        .unwrap()
        .contains("TOML"));
}

#[test]
fn structured_service_actions_preserve_json_and_report_failures_without_starting_host_services() {
    let profile = Profile::new();
    for action in ["install", "enable", "disable", "stop", "status"] {
        profile.json(&["service", action], true);
    }
    profile.json(&["service", "start"], false);
    profile.json(&["apply"], false);
    let output = profile
        .command()
        .args(["service", "enable", "--json"])
        .env("UKVM_TEST_SYSTEMCTL_FAIL", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("failed"));
    let log = fs::read_to_string(profile.dir.path().join("systemctl.log")).unwrap();
    assert!(!log.contains("--user restart ukvm.service"));
    assert!(!log.contains("--user start ukvm.service"));
}

struct PairProcess {
    child: Child,
    output: BufReader<std::process::ChildStdout>,
}
impl PairProcess {
    fn new(profile: &Profile, args: &[&str]) -> Self {
        let mut child = profile
            .command()
            .arg("pair")
            .args(args)
            .arg("--json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self { child, output }
    }
    fn event(&mut self, wanted: &str) -> Value {
        loop {
            let mut line = String::new();
            assert!(self.output.read_line(&mut line).unwrap() > 0);
            let event: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(event["schema_version"], 1);
            if event["event"] == wanted {
                return event;
            }
            assert_ne!(
                event["event"], "result",
                "unexpected pairing result {event}"
            );
        }
    }
    fn reply(&mut self, offer: &Value, accept: bool) {
        writeln!(self.child.stdin.as_mut().unwrap(), "{}", json!({"accept": accept, "remote_node_id": offer["data"]["remote_node_id"], "verification_code": offer["data"]["verification_code"]})).unwrap();
    }
}
impl Drop for PairProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn structured_pairing_requires_both_explicit_code_bound_approvals() {
    for accept in [false, true] {
        let a = Profile::new();
        let b = Profile::new();
        let mut responder = PairProcess::new(&b, &["--bind", "127.0.0.1:0"]);
        let listening = responder.event("listening");
        let endpoint = listening["data"]["endpoint"].as_str().unwrap();
        let mut initiator = PairProcess::new(
            &a,
            &[
                endpoint,
                "--expected-node-id",
                listening["data"]["node_id"].as_str().unwrap(),
            ],
        );
        let offer_a = initiator.event("confirmation_required");
        let offer_b = responder.event("confirmation_required");
        assert_eq!(
            offer_a["data"]["verification_code"],
            offer_b["data"]["verification_code"]
        );
        initiator.reply(&offer_a, accept);
        responder.reply(&offer_b, true);
        assert_eq!(initiator.event("result")["ok"], accept);
        assert_eq!(responder.event("result")["ok"], accept);
        for profile in [&a, &b] {
            let peers = profile.json(&["peers"], true);
            let peers = peers["data"]["peers"].as_array().unwrap();
            assert_eq!(peers.len(), usize::from(accept));
            if accept {
                assert_eq!(peers[0]["permissions"]["control_this_node"], false);
            }
        }
    }
}

#[test]
fn apply_uses_one_managed_restart_and_rejects_foreground_ownership() {
    use std::os::unix::net::UnixListener;
    let profile = Profile::new();
    profile.add_peer("io");
    profile.json(&["layout", "place", "1", "0", "io"], true);
    let _wayland = UnixListener::bind(profile.dir.path().join("wayland-test")).unwrap();
    let output = profile
        .command()
        .args(["apply", "--json"])
        .env("WAYLAND_DISPLAY", "wayland-test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["data"]["mechanism"], "user_service_restart");
    let log_path = profile.dir.path().join("systemctl.log");
    let log = fs::read_to_string(&log_path).unwrap();
    assert_eq!(log.matches("--user restart ukvm.service").count(), 1);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.dir.path().join("ukvm/runtime.lock"))
        .unwrap();
    // flock mantiene ocupado el mismo inodo sin arrancar otro runtime.
    let mut holder = Command::new("flock")
        .args(["-x", "-n"])
        .arg(profile.dir.path().join("ukvm/runtime.lock"))
        .args(["sh", "-c", "printf ready; cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut bytes = [0; 5];
    std::io::Read::read_exact(holder.stdout.as_mut().unwrap(), &mut bytes).unwrap();
    let result = profile.json(&["apply"], false);
    drop(lock);
    holder.stdin.take();
    holder.wait().unwrap();
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("foreground/unknown runtime"));
    assert_eq!(
        fs::read_to_string(&log_path)
            .unwrap()
            .matches("--user restart ukvm.service")
            .count(),
        1
    );
}

#[test]
fn first_control_permission_allow_apply_and_deny_apply_use_managed_lifecycle() {
    use std::os::unix::net::UnixListener;
    let profile = Profile::new();
    profile.add_peer("spectrum");
    let _socket = UnixListener::bind(profile.dir.path().join("wayland-test")).unwrap();
    for (action, expected) in [("allow", true), ("deny", false)] {
        let permission = profile.json(&["peer", action, "spectrum", "control-this-node"], true);
        assert_eq!(
            permission["data"]["permissions"]["control_this_node"],
            expected
        );
        assert_eq!(permission["data"]["apply_required"], true);
        let output = profile
            .command()
            .args(["apply", "--json"])
            .env("WAYLAND_DISPLAY", "wayland-test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["data"]["applied"], true);
        assert_eq!(result["data"]["mechanism"], "user_service_restart");
        let store = TrustStore::load(&profile.dir.path().join("state/ukvm")).unwrap();
        assert_eq!(
            store
                .find("spectrum")
                .unwrap()
                .permissions
                .control_this_node,
            expected
        );
    }
    let log = fs::read_to_string(profile.dir.path().join("systemctl.log")).unwrap();
    assert_eq!(log.matches("--user restart ukvm.service").count(), 2);
}

#[test]
fn setup_can_finish_local_preparation_without_a_peer_or_portal() {
    if !std::path::Path::new("/usr/bin/script").exists() {
        return;
    }
    let profile = Profile::new();
    let binary = env!("CARGO_BIN_EXE_ukvm").replace('\'', "'\\''");
    let base = profile.command();
    let mut script = Command::new("script");
    script.envs(
        base.get_envs()
            .filter_map(|(key, value)| value.map(|value| (key, value))),
    );
    script.env_remove("WAYLAND_DISPLAY");
    script
        .args(["-qefc", &format!("exec '{binary}' setup"), "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = script.spawn().unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"y\nn\ny\ny\nn\nn\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Platform readiness"));
    assert!(text.contains("Local setup complete"));
    assert!(text.contains("Trusted machines: 0"));
    assert!(text.contains("Current configuration"));
    assert!(text.contains("unavailable"));
    assert!(profile
        .dir
        .path()
        .join("config/systemd/user/ukvm.service")
        .exists());
    let log = fs::read_to_string(profile.dir.path().join("systemctl.log")).unwrap();
    assert!(log.contains("--user enable ukvm.service"));
    assert!(!log.contains("--user restart ukvm.service"));
}

#[test]
fn local_setup_initializes_without_trust_consent_or_service_and_is_idempotent() {
    let profile = Profile::new();
    let first = profile.json(&["setup", "--local", "--name", "My workstation"], true);
    assert_eq!(first["data"]["initialized"], true);
    assert_eq!(first["data"]["consent_requested"], false);
    assert_eq!(first["data"]["service_started"], false);
    assert_eq!(first["data"]["trust_changed"], false);
    let id = first["data"]["node"]["node_id"].clone();
    assert_eq!(id.as_str().unwrap().len(), 64);
    assert!(!profile.dir.path().join("systemctl.log").exists());
    let bytes = profile.configuration();
    let again = profile.json(&["setup", "--local"], true);
    assert_eq!(again["data"]["node"]["node_id"], id);
    assert_eq!(again["data"]["configuration_changed"], false);
    assert_eq!(again["data"]["apply_required"], false);
    assert_eq!(profile.configuration(), bytes);
}

#[test]
fn local_setup_preserves_trust_permissions_and_topology_when_renaming() {
    let profile = Profile::new();
    profile.json(&["setup", "--local"], true);
    let id = profile.add_peer("Existing peer");
    profile.json(&["layout", "place", "1", "0", &id], true);
    profile.json(&["peer", "allow", &id, "control-this-node"], true);
    let before = profile.json(&["peers"], true);
    profile.json(&["setup", "--local", "--name", "Renamed"], true);
    assert_eq!(profile.json(&["peers"], true), before);
}

#[test]
fn local_setup_rejects_invalid_names_and_pairing_flags_before_writes() {
    let profile = Profile::new();
    for args in [
        vec!["setup", "--local", "--name", ""],
        vec!["setup", "--local", "--name", "bad\nname"],
        vec!["setup", "--local", "--yes"],
        vec!["setup"],
    ] {
        profile.json(&args, false);
    }
    assert!(!profile.dir.path().join("data/ukvm/node.der").exists());
    assert!(!profile.dir.path().join("config/ukvm/config.toml").exists());
}

#[test]
fn packaged_portal_launcher_refuses_unready_session_without_executing_portal() {
    let profile = Profile::new();
    for action in ["backend", "frontend", "check-backend", "check-frontend"] {
        let mut command = profile.command();
        command = {
            let mut helper = Command::new(env!("CARGO_BIN_EXE_ukvm-portal-session"));
            helper.envs(
                command
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            );
            helper.env_remove("WAYLAND_DISPLAY");
            helper
        };
        command.arg(action);
        if action == "backend" || action == "frontend" {
            command.arg("/bin/true");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(75));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Portal not ready"));
    }
}

fn remembered_portal(profile: &Profile, secret: &str) -> std::path::PathBuf {
    let dir = profile.dir.path().join("state/ukvm/portal-authorization");
    fs::create_dir_all(&dir).unwrap();
    for path in [dir.parent().unwrap(), dir.as_path()] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = dir.join("remote-desktop.json");
    fs::write(&path, serde_json::to_vec(&json!({"format_version":1,"context":{"desktop":std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().to_lowercase(),"version":2},"token":secret,"authorized":false,"reason":"remembered"})).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

#[test]
fn desktop_authorization_status_and_reset_are_safe_and_keep_peer_policy() {
    let profile = Profile::new();
    let peer = profile.add_peer("independent-peer");
    profile.json(&["peer", "allow", &peer, "control-this-node"], true);
    let before = profile.json(&["peers"], true);
    let path = remembered_portal(&profile, "never-print-this-restore-token");
    let state = profile.json(&["authorization", "status"], true);
    assert_eq!(state["data"]["remote_desktop"]["state"], "persistent");
    assert!(!state.to_string().contains("never-print-this-restore-token"));
    let status = profile.json(&["status"], true);
    assert_eq!(
        status["data"]["desktop_authorization"]["remote_desktop"]["remembered"],
        true
    );
    assert!(!status
        .to_string()
        .contains("never-print-this-restore-token"));
    let human = profile
        .command()
        .args(["authorization", "status"])
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(!String::from_utf8_lossy(&human.stdout).contains("never-print-this-restore-token"));
    assert!(!String::from_utf8_lossy(&human.stderr).contains("never-print-this-restore-token"));
    profile.json(&["authorization", "reset"], false);
    assert!(path.exists());
    let reset = profile.json(&["authorization", "reset", "--yes"], true);
    assert_eq!(reset["data"]["trust_changed"], false);
    assert_eq!(reset["data"]["service_restarted"], false);
    assert_eq!(
        reset["data"]["desktop_authorization"]["remote_desktop"]["state"],
        "needs_approval"
    );
    assert!(!path.exists());
    assert_eq!(before, profile.json(&["peers"], true));
    assert!(fs::read_to_string(profile.dir.path().join("systemctl.log"))
        .unwrap()
        .lines()
        .all(|line| line.contains("show")));
}

#[test]
fn remembered_portal_never_grants_an_unpermissioned_peer_control() {
    let profile = Profile::new();
    let peer = profile.add_peer("trusted-without-control");
    remembered_portal(&profile, "desktop-grant-is-not-peer-trust");
    let value = profile.json(&["peer", "permissions", &peer], true);
    assert_eq!(value["data"]["permissions"]["control_this_node"], false);
    let status = profile.json(&["status"], true);
    assert_eq!(
        status["data"]["desktop_authorization"]["remote_desktop"]["remembered"],
        true
    );
    assert_eq!(
        status["data"]["peers"][0]["permissions"]["control_this_node"],
        false
    );
}

#[test]
fn corrupt_portal_state_reports_safe_status_and_resets_without_other_changes() {
    let profile = Profile::new();
    let path = remembered_portal(&profile, "private-token");
    fs::write(&path, "private-token invalid json").unwrap();
    let value = profile.json(&["authorization", "status"], true);
    assert_eq!(value["data"]["remote_desktop"]["reason"], "corrupt");
    assert!(!value.to_string().contains("private-token"));
    profile.json(&["authorization", "reset", "--yes"], true);
    assert!(!path.exists());
}

#[test]
fn reset_refuses_foreground_runtime_instead_of_leaving_an_active_grant() {
    let profile = Profile::new();
    let path = remembered_portal(&profile, "private-token");
    let runtime = profile.dir.path().join("ukvm");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let lock_path = runtime.join("runtime.lock");
    fs::write(&lock_path, "").unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut holder = Command::new("flock")
        .args(["-x", "-n"])
        .arg(&lock_path)
        .args(["sh", "-c", "printf ready; cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut bytes = [0; 5];
    std::io::Read::read_exact(holder.stdout.as_mut().unwrap(), &mut bytes).unwrap();
    let result = profile.json(&["authorization", "reset", "--yes"], false);
    holder.stdin.take();
    holder.wait().unwrap();
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("ownership is uncertain"));
    assert!(path.exists());
    assert!(!profile.dir.path().join("systemctl.log").exists());
}

#[test]
fn reset_stops_only_the_owned_managed_runtime_without_restarting_or_changing_trust() {
    let profile = Profile::new();
    profile.add_peer("retained-peer");
    let before = profile.json(&["peers"], true);
    let path = remembered_portal(&profile, "private-managed-token");
    let runtime = profile.dir.path().join("ukvm");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let lock_path = runtime.join("runtime.lock");
    fs::write(&lock_path, "").unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut holder = Command::new("flock")
        .args(["-F", "-x", "-n"])
        .arg(&lock_path)
        .args(["sh", "-c", "printf ready; read ignored"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut bytes = [0; 5];
    std::io::Read::read_exact(holder.stdout.as_mut().unwrap(), &mut bytes).unwrap();
    let snapshot = ukvm_runtime::ownership::RuntimeSnapshot {
        pid: holder.id(),
        updated: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        ..Default::default()
    };
    fs::write(
        runtime.join("status.toml"),
        toml::to_string(&snapshot).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        runtime.join("status.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(profile.dir.path().join("bin/systemctl"), r#"#!/bin/sh
printf '%s\n' "$*" >> "$UKVM_TEST_LOG"
case "$*" in
  *show*) printf 'LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=enabled\nMainPID=%s\n' "$UKVM_TEST_HOLDER_PID";;
  *stop*) kill -TERM "$UKVM_TEST_HOLDER_PID";;
esac
"#).unwrap();
    let output = profile
        .command()
        .env("UKVM_TEST_HOLDER_PID", holder.id().to_string())
        .args(["authorization", "reset", "--yes", "--json"])
        .output()
        .unwrap();
    holder.stdin.take();
    holder.wait().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["service_stopped"], true);
    assert_eq!(value["data"]["service_restarted"], false);
    assert!(!path.exists());
    assert_eq!(before, profile.json(&["peers"], true));
    let log = fs::read_to_string(profile.dir.path().join("systemctl.log")).unwrap();
    assert_eq!(
        log.lines().filter(|line| line.contains(" stop ")).count(),
        1
    );
    assert!(log
        .lines()
        .all(|line| line.contains("show") || line.contains(" stop ")));
}

#[test]
fn clipboard_defaults_off_and_permission_is_live_and_separate() {
    let profile = Profile::new();
    profile.add_peer("io");
    let status = profile.json(&["status"], true);
    assert_eq!(status["schema_version"], 1);
    assert_eq!(
        status["data"]["peers"][0]["permissions"]["share_clipboard"],
        false
    );
    assert_eq!(status["data"]["clipboard"]["enabled"], false);
    let result = profile.json(&["peer", "allow", "io", "share-clipboard"], true);
    assert_eq!(result["data"]["permissions"]["share_clipboard"], true);
    assert_eq!(result["data"]["permissions"]["control_this_node"], false);
    assert_eq!(result["data"]["apply_required"], false);
    assert!(result["data"]["apply_command"].is_null());
    let status = profile.json(&["status"], true);
    assert_eq!(status["data"]["clipboard"]["enabled"], true);
    assert_eq!(status["data"]["clipboard"]["max_text_bytes"], 65536);
    assert!(status["data"]["clipboard"].get("text").is_none());
    assert!(status["data"]["clipboard"].get("contents").is_none());
    profile.json(&["peer", "deny", "io", "share-clipboard"], true);
    assert_eq!(
        profile.json(&["status"], true)["data"]["clipboard"]["enabled"],
        false
    );
}

#[test]
fn older_running_runtime_requires_restart_before_clipboard_permission_edits() {
    let profile = Profile::new();
    profile.add_peer("io");
    let runtime = profile.dir.path().join("ukvm");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let lock_path = runtime.join("runtime.lock");
    fs::write(&lock_path, "").unwrap();
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut holder = Command::new("flock")
        .args(["-F", "-x", "-n"])
        .arg(&lock_path)
        .args(["sh", "-c", "printf ready; read ignored"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut bytes = [0; 5];
    std::io::Read::read_exact(holder.stdout.as_mut().unwrap(), &mut bytes).unwrap();
    let snapshot = ukvm_runtime::ownership::RuntimeSnapshot {
        pid: holder.id(),
        updated: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        ..Default::default()
    };
    fs::write(
        runtime.join("status.toml"),
        toml::to_string(&snapshot).unwrap(),
    )
    .unwrap();
    fs::set_permissions(
        runtime.join("status.toml"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert_eq!(
        profile.json(&["status"], true)["data"]["clipboard"]["state"],
        "restart_required"
    );
    profile.json(&["peer", "allow", "io", "share-clipboard"], false);
    assert_eq!(
        profile.json(&["peers"], true)["data"]["peers"][0]["permissions"]["share_clipboard"],
        false
    );
    assert_eq!(
        profile.json(&["files", "status"], true)["data"]["state"],
        "restart_required"
    );
    profile.json(&["peer", "allow", "io", "receive-files"], false);
    assert_eq!(
        profile.json(&["peers"], true)["data"]["peers"][0]["permissions"]["receive_files"],
        false
    );
    holder.stdin.take();
    holder.wait().unwrap();
}
#[test]
fn files_default_deny_live_permission_private_status_and_directory_apply() {
    let p = Profile::new();
    p.add_peer("recipient");
    let status = p.json(&["status"], true);
    assert_eq!(
        status["data"]["peers"][0]["permissions"]["receive_files"],
        false
    );
    let changed = p.json(&["peer", "allow", "recipient", "receive-files"], true);
    assert_eq!(changed["data"]["permissions"]["receive_files"], true);
    assert_eq!(changed["data"]["permissions"]["control_this_node"], false);
    assert_eq!(changed["data"]["permissions"]["share_clipboard"], false);
    assert_eq!(changed["data"]["apply_required"], false);
    p.json(&["peer", "deny", "recipient", "receive-files"], true);
    let files = p.json(&["files", "status"], true);
    assert_eq!(files["data"]["protocol_version"], 1);
    assert!(files["data"].get("contents").is_none());
    assert!(files["data"]["transfers"].as_array().unwrap().is_empty());
    let path = p.dir.path().join("receive");
    let changed = p.json(&["files", "directory", path.to_str().unwrap()], true);
    assert_eq!(changed["data"]["apply_required"], true);
    p.json(&["files", "directory", "relative"], false);
    p.json(&["files", "cancel", "../escape"], false);
}
#[test]
fn actual_cli_send_jsonl_pinned_tls_and_receiver_hash_verification() {
    use ukvm_network::{
        files,
        transport::{self, RuntimeConnection},
    };
    let p = Profile::new();
    let a = Identity::load_or_create(&p.dir.path().join("data/ukvm")).unwrap();
    let b = Identity::load_or_create(&p.dir.path().join("receiver-identity")).unwrap();
    let trust = p.dir.path().join("state/ukvm");
    let bd = p.dir.path().join("receiver-trust");
    let root = p.dir.path().join("received");
    let expected = b"binary\x00secret-file-contents\xff";
    let source = p.dir.path().join("a file.txt");
    fs::write(&source, expected).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let mut ast = TrustStore::default();
    ast.import(
        &trust,
        "receiver".into(),
        Some(listener.local_addr().unwrap().to_string()),
        b.tls_cert().as_ref(),
    )
    .unwrap();
    let mut bst = TrustStore::default();
    bst.import(&bd, "sender".into(), None, a.tls_cert().as_ref())
        .unwrap();
    bst.set_permission(
        &bd,
        &a.id,
        ukvm_network::trust::SessionPermission::ReceiveFiles,
        true,
    )
    .unwrap();
    let mut command = p.command();
    command.args(["send", source.to_str().unwrap(), "receiver", "--json"]);
    let client = std::thread::spawn(move || command.output().unwrap());
    let output = root.clone();
    runtime.block_on(async {
        let (tcp, _) = listener.accept().await.unwrap();
        let RuntimeConnection::Files { mut stream, .. } =
            transport::accept_runtime(tcp, &b, &bst, &bd, "receiver")
                .await
                .unwrap()
        else {
            panic!("unexpected channel")
        };
        files::receive(
            &mut stream,
            std::sync::Arc::new(|| Ok(())),
            std::sync::Arc::new(|_| Ok(())),
            move |offer| {
                Ok(Box::new(ukvm_platform::files::linux::Receiver::new(
                    &output, offer,
                )?))
            },
        )
        .await
        .unwrap();
    });
    let result = client.join().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read(root.join("a file.txt")).unwrap(), expected);
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(!stdout.contains("secret-file-contents"));
    let records = stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.first().unwrap()["event"], "started");
    assert_eq!(records.last().unwrap()["event"], "result");
    assert_eq!(records.last().unwrap()["ok"], true);
    assert!(records.iter().all(|r| r["schema_version"] == 1));
}
