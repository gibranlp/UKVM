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
        fixture.executable(
            "bin/systemctl",
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$UKVM_TEST_LOG\"\n",
        );
        fixture.executable(
            "bin/git",
            "#!/bin/sh\nprintf '%s\\n' \"${UKVM_TEST_REVISION}\"\n",
        );
        fixture.executable(
            "helper",
            "#!/bin/sh\nexit \"${UKVM_TEST_HELPER_FAIL:-0}\"\n",
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
            .env("UKVM_TRIAL_HELPER", self.path("helper"))
            .env("UKVM_TEST_LOG", self.path("systemctl.log"))
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
    let units = fixture.path("runtime/systemd/user");
    let backend =
        fs::read_to_string(units.join("xdg-desktop-portal-hyprland.service.d/ukvm-trial.conf"))
            .unwrap();
    let frontend =
        fs::read_to_string(units.join("xdg-desktop-portal.service.d/ukvm-trial.conf")).unwrap();
    assert!(backend.contains("ConditionEnvironment=\n"));
    assert!(backend.contains("Wants=xdg-desktop-portal.service"));
    assert!(backend.contains("check-backend"));
    assert!(backend.contains("build %% quoted\\\"/xdg-desktop-portal-hyprland"));
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
    assert_eq!(log, "--user daemon-reload\n--user stop xdg-desktop-portal.service xdg-desktop-portal-hyprland.service\n--user reset-failed xdg-desktop-portal-hyprland.service xdg-desktop-portal.service\n--user restart xdg-desktop-portal.service\n");
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
    assert!(!fixture.path("runtime/ukvm-xdph-trial").exists());
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
    assert!(!fixture.path("runtime/ukvm-xdph-trial").exists());
    let unit = fixture.path("runtime/systemd/user/xdg-desktop-portal.service.d/ukvm-trial.conf");
    fs::create_dir_all(unit.parent().unwrap()).unwrap();
    fs::write(&unit, "unrelated").unwrap();
    assert!(!fixture.run("stop").status.success());
    assert_eq!(fs::read_to_string(unit).unwrap(), "unrelated");
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
