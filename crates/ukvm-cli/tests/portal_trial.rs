#![cfg(target_os = "linux")]
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

const REVISION: &str = "0204ffbbcb3cef72af76804cdf59f114f2e2f6d9";
struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for path in [
            "bin",
            "runtime",
            "unit-state",
            "source",
            "build % quoted\"/hyprland-share-picker",
            "config/xdg-desktop-portal",
        ] {
            fs::create_dir_all(dir.path().join(path)).unwrap();
        }
        fs::set_permissions(
            dir.path().join("runtime"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let fixture = Self { dir };
        fixture.executable("bin/systemctl", r#"#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$UKVM_TEST_LOG"
shift
action=$1
shift
backend=xdg-desktop-portal-hyprland.service
frontend=xdg-desktop-portal.service
state() { cat "$UKVM_TEST_STATE/$1" 2>/dev/null || printf unloaded; }
put() { printf '%s\n' "$2" > "$UKVM_TEST_STATE/$1"; }
case $action in
    daemon-reload) ;;
    stop)
        for unit in "$@"; do
            if [[ ${UKVM_TEST_KEEP_FAILED:-0} == 1 && $(state "$unit") == failed ]]; then continue; fi
            put "$unit" unloaded
        done ;;
    show)
        if [[ $1 == --property=ActiveState ]]; then
            value=$(state "${@: -1}")
            [[ $value != unloaded ]] || value=inactive
            printf '%s\n' "$value"
        else
            for unit in "$backend" "$frontend"; do printf 'Id=%s\nActiveState=%s\n' "$unit" "$(state "$unit")"; done
        fi ;;
    reset-failed)
        for unit in "$@"; do
            if [[ $(state "$unit") == unloaded || ${UKVM_TEST_RESET_FAIL:-0} == 1 ]]; then
                printf 'Failed to reset failed state: Unit %s not loaded or manager error.\n' "$unit" >&2
                exit 1
            fi
            put "$unit" inactive
        done ;;
    restart)
        if [[ -f $XDG_CONFIG_HOME/systemd/user/$frontend.d/ukvm-trial.conf ]]; then
            case ${UKVM_TEST_TRIAL_START:-ok} in
                fail) put "$backend" failed; exit 1;;
                skip) exit 0;;
                backend-only) put "$backend" active; exit 0;;
            esac
            put "$backend" active
            put "$frontend" active
        elif [[ ${UKVM_TEST_RESTORE_FAIL:-0} == 1 && $1 == "$backend" ]]; then
            put "$backend" failed
            exit 1
        else
            put "$1" active
        fi ;;
    is-active) [[ $(state "${@: -1}") == active ]];;
    *) printf 'Unexpected systemctl action: %s\n' "$action" >&2; exit 2;;
esac
"#);
        fixture.executable(
            "bin/git",
            "#!/bin/sh\nprintf '%s\\n' \"${UKVM_TEST_REVISION}\"\n",
        );
        fixture.executable(
            "helper",
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$UKVM_TEST_HELPER_LOG\"\ncase $1 in \"${UKVM_TEST_HELPER_FAIL_ACTION:-never}\") exit 75;; esac\nexit \"${UKVM_TEST_HELPER_FAIL:-0}\"\n",
        );
        fixture.executable(
            "build % quoted\"/xdg-desktop-portal-hyprland",
            "#!/bin/sh\nexit 0\n",
        );
        fixture.executable(
            "build % quoted\"/hyprland-share-picker/hyprland-share-picker",
            "#!/bin/sh\nexit 0\n",
        );
        fs::write(
            fixture.path("source/hyprland.portal"),
            "[portal]\nInterfaces=org.freedesktop.impl.portal.RemoteDesktop;\n",
        )
        .unwrap();
        fs::write(
            fixture.path("config/xdg-desktop-portal/hyprland-portals.conf"),
            "[preferred]\ndefault=hyprland;gtk\n",
        )
        .unwrap();
        fixture
    }
    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }
    fn executable(&self, path: &str, text: &str) {
        fs::write(self.path(path), text).unwrap();
        fs::set_permissions(self.path(path), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/wayland-portal-trial"
        ));
        command
            .arg(action)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path("bin").display()),
            )
            .env("XDG_RUNTIME_DIR", self.path("runtime"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("UKVM_TRIAL_HELPER", self.path("helper"))
            .env("UKVM_TEST_LOG", self.path("systemctl.log"))
            .env("UKVM_TEST_STATE", self.path("unit-state"))
            .env("UKVM_TEST_HELPER_LOG", self.path("helper.log"))
            .env("UKVM_TEST_REVISION", REVISION);
        if action == "start" {
            command
                .arg(self.path("source"))
                .arg(self.path("build % quoted\""));
        }
        command
    }
    fn run(&self, action: &str) -> std::process::Output {
        self.command(action).output().unwrap()
    }
    fn assert_stock_restored(&self) {
        assert!(!self.path("data/ukvm/portal-trial").exists());
        for unit in [
            "xdg-desktop-portal-hyprland.service",
            "xdg-desktop-portal.service",
        ] {
            assert_eq!(
                fs::read_to_string(self.path(&format!("unit-state/{unit}"))).unwrap(),
                "active\n"
            );
            assert!(!self
                .path(&format!("config/systemd/user/{unit}.d/ukvm-trial.conf"))
                .exists());
        }
    }
}

#[test]
fn trial_units_order_fresh_backend_before_frontend_and_rollback_keeps_other_overrides() {
    if !std::path::Path::new("/usr/lib/xdg-desktop-portal").exists() {
        return;
    }
    let fixture = Fixture::new();
    let output = fixture.run("start");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let units = fixture.path("config/systemd/user");
    let backend =
        fs::read_to_string(units.join("xdg-desktop-portal-hyprland.service.d/ukvm-trial.conf"))
            .unwrap();
    let frontend =
        fs::read_to_string(units.join("xdg-desktop-portal.service.d/ukvm-trial.conf")).unwrap();
    assert!(backend.contains("ConditionEnvironment=\n"));
    assert!(backend.contains("Wants=xdg-desktop-portal.service"));
    assert!(backend.contains("check-backend"));
    assert!(backend.contains("data/ukvm/portal-trial/xdg-desktop-portal-hyprland"));
    assert!(!backend.contains("build %% quoted"));
    assert!(
        frontend.contains("BindsTo=graphical-session.target xdg-desktop-portal-hyprland.service")
    );
    assert!(frontend.contains("After=xdg-desktop-portal-hyprland.service"));
    assert!(frontend.contains("check-frontend"));
    for text in [&backend, &frontend] {
        assert!(text.contains("Environment=XDG_RUNTIME_DIR=%t"));
        assert!(text.contains("RestartSec=5"));
    }
    let log = fs::read_to_string(fixture.path("systemctl.log")).unwrap();
    assert!(
        !log.contains("reset-failed"),
        "unloaded/static units need no failure reset: {log}"
    );
    assert!(log.contains("--user restart xdg-desktop-portal.service"));
    assert_eq!(
        fs::read_to_string(fixture.path("data/ukvm/portal-trial/state")).unwrap(),
        "active\n"
    );
    assert!(fixture.run("status").status.success());
    let bad = fixture
        .command("status")
        .env("UKVM_TEST_HELPER_FAIL", "75")
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not ready for this login"));

    // El verificador lee unidades aisladas; nunca opera el administrador del usuario.
    if std::path::Path::new("/usr/bin/systemd-analyze").exists() {
        for name in [
            "xdg-desktop-portal-hyprland.service",
            "xdg-desktop-portal.service",
        ] {
            fs::copy(format!("/usr/lib/systemd/user/{name}"), units.join(name)).unwrap();
        }
        let output = Command::new("systemd-analyze")
            .args(["--user", "verify"])
            .arg(units.join("xdg-desktop-portal-hyprland.service"))
            .arg(units.join("xdg-desktop-portal.service"))
            .env("XDG_RUNTIME_DIR", fixture.path("runtime"))
            .env(
                "SYSTEMD_UNIT_PATH",
                format!("{}:/usr/lib/systemd/user", units.display()),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let unrelated = units.join("xdg-desktop-portal.service.d/unrelated.conf");
    fs::write(&unrelated, "[Service]\nUMask=0077\n").unwrap();
    assert!(fixture.run("stop").status.success());
    assert!(unrelated.exists());
    assert!(!fixture.path("data/ukvm/portal-trial").exists());
    assert!(!units
        .join("xdg-desktop-portal.service.d/ukvm-trial.conf")
        .exists());
    assert!(!units
        .join("xdg-desktop-portal-hyprland.service.d/ukvm-trial.conf")
        .exists());
}

#[test]
fn trial_refuses_other_revision_and_unowned_rollback() {
    let fixture = Fixture::new();
    let output = fixture
        .command("start")
        .env("UKVM_TEST_REVISION", "unreviewed")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!fixture.path("data/ukvm/portal-trial").exists());
    let unit = fixture.path("config/systemd/user/xdg-desktop-portal.service.d/ukvm-trial.conf");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unrelated").unwrap();
    assert!(!fixture.run("stop").status.success());
    assert_eq!(fs::read_to_string(&unit).unwrap(), "unrelated");
    fs::remove_file(&unit).unwrap();
    std::os::unix::fs::symlink("/nonexistent/ukvm-unrelated-override", &unit).unwrap();
    assert!(!fixture.run("start").status.success());
    assert!(fs::symlink_metadata(&unit)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn legacy_trial_can_be_removed_without_new_helper() {
    let fixture = Fixture::new();
    let trial = fixture.path("runtime/ukvm-xdph-trial");
    fs::create_dir_all(trial.join("portals")).unwrap();
    fs::write(trial.join("owned-by-ukvm-trial"), REVISION).unwrap();
    let status = fixture.run("status");
    assert!(!status.status.success());
    assert!(String::from_utf8_lossy(&status.stderr).contains("Legacy/incomplete"));
    assert!(fixture.run("stop").status.success());
    assert!(!trial.exists());
}

#[test]
fn unloaded_dbus_units_and_partial_trial_failures_restore_both_stock_services() {
    if !std::path::Path::new("/usr/lib/xdg-desktop-portal").exists() {
        return;
    }
    for mode in ["fail", "skip", "backend-only", "capability-failure"] {
        let fixture = Fixture::new();
        let mut command = fixture.command("start");
        if mode == "capability-failure" {
            command.env("UKVM_TEST_HELPER_FAIL_ACTION", "check-frontend");
        } else {
            command.env("UKVM_TEST_TRIAL_START", mode);
        }
        let output = command.output().unwrap();
        assert!(
            !output.status.success(),
            "{mode} must fail instead of reporting trial success"
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Trial active."));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Trial setup failed"));
        fixture.assert_stock_restored();
        let log = fs::read_to_string(fixture.path("systemctl.log")).unwrap();
        assert!(!log.contains("reset-failed"), "{mode}: {log}");
        assert!(log.contains("--user restart xdg-desktop-portal-hyprland.service"));
        assert!(fixture.run("status").status.success());
    }
}

#[test]
fn restoration_errors_keep_recovery_state_and_attempt_both_services() {
    if !std::path::Path::new("/usr/lib/xdg-desktop-portal").exists() {
        return;
    }
    let fixture = Fixture::new();
    let output = fixture
        .command("start")
        .env("UKVM_TEST_TRIAL_START", "fail")
        .env("UKVM_TEST_RESTORE_FAIL", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(fixture.path("data/ukvm/portal-trial/state")).unwrap(),
        "broken\n"
    );
    let log = fs::read_to_string(fixture.path("systemctl.log")).unwrap();
    assert!(log.contains("--user restart xdg-desktop-portal-hyprland.service\n--user restart xdg-desktop-portal.service"));
    assert!(!fixture.run("status").status.success());
    assert!(fixture.run("stop").status.success());
    fixture.assert_stock_restored();
}

#[test]
fn genuine_reset_failure_is_reported_and_can_be_recovered() {
    if !std::path::Path::new("/usr/lib/xdg-desktop-portal").exists() {
        return;
    }
    let fixture = Fixture::new();
    fs::write(
        fixture.path("unit-state/xdg-desktop-portal-hyprland.service"),
        "failed\n",
    )
    .unwrap();
    let output = fixture
        .command("start")
        .env("UKVM_TEST_KEEP_FAILED", "1")
        .env("UKVM_TEST_RESET_FAIL", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("manager error"));
    assert!(fixture
        .path("data/ukvm/portal-trial/owned-by-ukvm-trial")
        .exists());
    assert!(fixture.run("stop").status.success());
    fixture.assert_stock_restored();
}

#[test]
fn status_detects_inactive_units_and_cleanup_refuses_modified_owned_paths() {
    if !std::path::Path::new("/usr/lib/xdg-desktop-portal").exists() {
        return;
    }
    let fixture = Fixture::new();
    let initial = fixture.run("status");
    assert!(!initial.status.success());
    assert!(String::from_utf8_lossy(&initial.stderr).contains("Portal stack inactive/broken"));
    assert!(fixture.run("start").status.success());
    fs::write(
        fixture.path("unit-state/xdg-desktop-portal.service"),
        "inactive\n",
    )
    .unwrap();
    let partial = fixture.run("status");
    assert!(!partial.status.success());
    assert!(String::from_utf8_lossy(&partial.stderr).contains("Trial state: broken"));
    fs::write(
        fixture.path("unit-state/xdg-desktop-portal-hyprland.service"),
        "inactive\n",
    )
    .unwrap();
    let inactive = fixture.run("status");
    assert!(!inactive.status.success());
    assert!(String::from_utf8_lossy(&inactive.stdout).contains("Trial state: inactive"));
    let path = fixture.path("config/systemd/user/xdg-desktop-portal.service.d/ukvm-trial.conf");
    fs::write(&path, "[Service]\nExecStart=/unrelated\n").unwrap();
    let log = fs::read_to_string(fixture.path("systemctl.log")).unwrap();
    let output = fixture.run("stop");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("override was modified"));
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        "[Service]\nExecStart=/unrelated\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.path("systemctl.log")).unwrap(),
        log
    );
}

#[test]
fn persistent_trial_survives_runtime_and_manager_recreation_without_original_build() {
    if !std::path::Path::new("/usr/lib/xdg-desktop-portal").exists() {
        return;
    }
    for manager_state in ["inactive", "unloaded"] {
        let fixture = Fixture::new();
        assert!(fixture.run("start").status.success());
        let trial = fixture.path("data/ukvm/portal-trial");
        let backend_override = fixture
            .path("config/systemd/user/xdg-desktop-portal-hyprland.service.d/ukvm-trial.conf");
        let definition = fs::read(&backend_override).unwrap();
        for (installed, original) in [
            ("session-helper", "helper"),
            (
                "xdg-desktop-portal-hyprland",
                "build % quoted\"/xdg-desktop-portal-hyprland",
            ),
            (
                "hyprland-share-picker",
                "build % quoted\"/hyprland-share-picker/hyprland-share-picker",
            ),
        ] {
            assert_eq!(
                fs::read(trial.join(installed)).unwrap(),
                fs::read(fixture.path(original)).unwrap()
            );
        }
        fs::remove_dir_all(fixture.path("runtime")).unwrap();
        fs::create_dir(fixture.path("new-runtime")).unwrap();
        fs::remove_dir_all(fixture.path("source")).unwrap();
        fs::remove_dir_all(fixture.path("build % quoted\"")).unwrap();
        fs::remove_file(fixture.path("helper")).unwrap();
        for unit in [
            "xdg-desktop-portal-hyprland.service",
            "xdg-desktop-portal.service",
        ] {
            fs::write(fixture.path(&format!("unit-state/{unit}")), manager_state).unwrap();
        }
        let status = fixture
            .command("status")
            .env("XDG_RUNTIME_DIR", fixture.path("new-runtime"))
            .output()
            .unwrap();
        assert!(!status.status.success());
        let text = String::from_utf8_lossy(&status.stdout);
        assert!(text.contains("installed (persistent)"), "{text}");
        assert!(text.contains("Trial state: inactive"), "{text}");
        assert_eq!(fs::read(&backend_override).unwrap(), definition);
        let activation = Command::new(fixture.path("bin/systemctl"))
            .args(["--user", "restart", "xdg-desktop-portal.service"])
            .env("XDG_CONFIG_HOME", fixture.path("config"))
            .env("XDG_RUNTIME_DIR", fixture.path("new-runtime"))
            .env("UKVM_TEST_STATE", fixture.path("unit-state"))
            .env("UKVM_TEST_LOG", fixture.path("systemctl.log"))
            .output()
            .unwrap();
        assert!(activation.status.success());
        let status = fixture
            .command("status")
            .env("XDG_RUNTIME_DIR", fixture.path("new-runtime"))
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(String::from_utf8_lossy(&status.stdout).contains("Trial state: active"));
        fs::write(fixture.path("helper.log"), "").unwrap();
        let stop = fixture
            .command("stop")
            .env("XDG_RUNTIME_DIR", fixture.path("new-runtime"))
            .output()
            .unwrap();
        assert!(
            stop.status.success(),
            "{}",
            String::from_utf8_lossy(&stop.stderr)
        );
        assert!(fs::read_to_string(fixture.path("helper.log"))
            .unwrap()
            .contains("prepare-environment"));
        fixture.assert_stock_restored();
    }
}

#[test]
fn missing_persistent_integration_is_broken_and_legacy_orphans_are_preserved() {
    if !std::path::Path::new("/usr/lib/xdg-desktop-portal").exists() {
        return;
    }
    for missing in [
        "data/ukvm/portal-trial/xdg-desktop-portal-hyprland",
        "data/ukvm/portal-trial/hyprland-share-picker",
        "config/systemd/user/xdg-desktop-portal.service.d/ukvm-trial.conf",
    ] {
        let fixture = Fixture::new();
        assert!(fixture.run("start").status.success());
        fs::remove_file(fixture.path(missing)).unwrap();
        let status = fixture.run("status");
        assert!(!status.status.success());
        assert!(String::from_utf8_lossy(&status.stderr)
            .contains("persistent integration/assets missing"));
        assert!(fixture.run("stop").status.success());
        fixture.assert_stock_restored();
    }
    let fixture = Fixture::new();
    let orphan = fixture.path("runtime/systemd/user/xdg-desktop-portal.service.d/ukvm-trial.conf");
    fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("/nonexistent/unrelated", &orphan).unwrap();
    for action in ["start", "status", "stop"] {
        assert!(!fixture.run(action).status.success());
        assert!(fs::symlink_metadata(&orphan)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
