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
