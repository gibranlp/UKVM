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
