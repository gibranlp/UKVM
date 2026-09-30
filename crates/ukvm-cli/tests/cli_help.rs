use std::process::Command;

#[test]
fn discovery_help_is_not_pairing_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_ukvm"))
        .args(["discover", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.starts_with("Usage: ukvm discover"));
    assert!(!help.contains("Usage: ukvm pair"));
}

#[test]
fn pairing_help_includes_interface_and_debug() {
    let output = Command::new(env!("CARGO_BIN_EXE_ukvm"))
        .args(["pair", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.starts_with("Usage: ukvm pair"));
    assert!(help.contains("--interface NAME"));
    assert!(help.contains("--debug"));
}

#[test]
fn version_identifies_discovery_build() {
    let output = Command::new(env!("CARGO_BIN_EXE_ukvm"))
        .arg("version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("discovery mDNS/DNS-SD"));
}

#[test]
fn setup_help_and_noninteractive_safety() {
    let binary = env!("CARGO_BIN_EXE_ukvm");
    let help = Command::new(binary)
        .args(["setup", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.starts_with("Usage: ukvm setup"));
    assert!(text.contains("--interface NAME"));
    assert!(text.contains("--host HOST"));
    let temp = tempfile::tempdir().unwrap();
    let rejected = Command::new(binary)
        .arg("setup")
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .env("XDG_DATA_HOME", temp.path().join("data"))
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(String::from_utf8(rejected.stderr)
        .unwrap()
        .contains("interactive terminal"));
    assert!(!temp.path().join("data/ukvm/node.key").exists());
}

#[test]
fn runtime_help_and_invalid_options_do_not_start_runtime() {
    let binary = env!("CARGO_BIN_EXE_ukvm");
    let output = Command::new(binary)
        .args(["run", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .starts_with("Usage: ukvm run [--verbose]"));
    let output = Command::new(binary)
        .args(["run", "--remote"])
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[cfg(target_os = "linux")]
#[test]
fn runtime_starts_and_sigint_releases_listener_without_portal_consent() {
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
        time::{Duration, Instant},
    };
    struct RuntimeChild(std::process::Child);
    impl Drop for RuntimeChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config/ukvm");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(
        config.join("config.toml"),
        "[node]\nname = \"runtime-test\"\n[network]\nlisten = \"127.0.0.1:0\"\n",
    )
    .unwrap();
    let mut child = RuntimeChild(
        Command::new(env!("CARGO_BIN_EXE_ukvm"))
            .args(["run", "--verbose"])
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_STATE_HOME", temp.path().join("state"))
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        tx.send(first).unwrap();
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut reader, &mut rest).unwrap();
        rest
    });
    let startup = rx.recv_timeout(Duration::from_secs(5));
    if startup.is_err() {
        let _ = child.0.kill();
        let _ = child.0.wait();
    }
    let startup = startup.unwrap();
    assert!(startup.starts_with("UKVM running as runtime-test"));
    let address = startup
        .split(" on ")
        .nth(1)
        .unwrap()
        .split(". Ctrl+C")
        .next()
        .unwrap();
    // The child has installed SIGINT handling before announcing readiness.
    assert!(Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.0.kill();
            let _ = child.0.wait();
            panic!("runtime did not shut down");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    assert!(reader.join().unwrap().contains("UKVM stopped."));
    assert!(std::net::TcpListener::bind(address).is_ok());
}
