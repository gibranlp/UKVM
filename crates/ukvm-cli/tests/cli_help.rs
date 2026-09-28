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
