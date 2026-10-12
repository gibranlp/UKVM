//! Distro-independent Linux binary/unit staging. No service is enabled or started.
use std::{
    fs,
    path::{Path, PathBuf},
    process::ExitCode,
};
fn home_prefix() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join(".local"))
        .ok_or("HOME is missing; specify --prefix".into())
}
fn stage(prefix: &Path, destdir: Option<&Path>) -> Result<PathBuf, String> {
    if !prefix.is_absolute()
        || prefix
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("--prefix must be an absolute path without '..'".into());
    }
    match destdir {
        Some(dir) if dir.is_absolute() => {
            Ok(dir.join(prefix.strip_prefix("/").map_err(|e| e.to_string())?))
        }
        Some(_) => Err("--destdir must be absolute".into()),
        None => Ok(prefix.to_owned()),
    }
}
fn install(source: &Path, prefix: &Path, destdir: Option<&Path>) -> Result<PathBuf, String> {
    let base = stage(prefix, destdir)?;
    let binary = base.join("bin/ukvm");
    let unit = base.join("lib/systemd/user/ukvm.service");
    let doc = base.join("share/doc/ukvm/SERVICE.md");
    let remote_doc = base.join("share/doc/ukvm/REMOTE_SESSIONS.md");
    let topology_doc = base.join("share/doc/ukvm/TOPOLOGY.md");
    let contract_doc = base.join("share/doc/ukvm/FRONTEND_CONTRACT.md");
    let schema_doc = base.join("share/doc/ukvm/cli-schema-v1.json");
    for path in [
        &binary,
        &unit,
        &doc,
        &topology_doc,
        &remote_doc,
        &contract_doc,
        &schema_doc,
    ] {
        fs::create_dir_all(path.parent().ok_or("invalid install path")?)
            .map_err(|e| e.to_string())?;
    }
    // Reject symlink destinations; publish the new executable atomically so a
    // running instance keeps its old inode until an explicit service restart.
    for path in [
        &binary,
        &unit,
        &doc,
        &topology_doc,
        &remote_doc,
        &contract_doc,
        &schema_doc,
    ] {
        if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file()) {
            return Err(format!(
                "refusing non-regular install destination {}",
                path.display()
            ));
        }
    }
    let pending = binary.with_extension(format!("pending-{}", std::process::id()));
    let mut input = fs::File::open(source).map_err(|e| e.to_string())?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        std::io::copy(&mut input, &mut output)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&pending, fs::Permissions::from_mode(0o755))?;
        }
        fs::rename(&pending, &binary)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result.map_err(|e| e.to_string())?;
    fs::write(
        &unit,
        super::service::unit(&prefix.join("bin/ukvm"), false)?,
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        &remote_doc,
        include_str!("../../../docs/REMOTE_SESSIONS.md"),
    )
    .map_err(|e| e.to_string())?;
    fs::write(&doc, include_str!("../../../docs/SERVICE.md")).map_err(|e| e.to_string())?;
    fs::write(&topology_doc, include_str!("../../../docs/TOPOLOGY.md"))
        .map_err(|e| e.to_string())?;
    fs::write(
        &contract_doc,
        include_str!("../../../docs/FRONTEND_CONTRACT.md"),
    )
    .map_err(|e| e.to_string())?;
    fs::write(
        &schema_doc,
        include_str!("../../../docs/cli-schema-v1.json"),
    )
    .map_err(|e| e.to_string())?;
    Ok(binary)
}
pub fn command(args: &[String]) -> ExitCode {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = args;
        return super::fail("installation currently supports Linux only");
    }
    #[cfg(target_os = "linux")]
    {
        let result = (|| -> Result<(), String> {
            if args == ["--help"] {
                println!("Usage: ukvm install [--prefix ABSOLUTE_PATH] [--destdir ABSOLUTE_PATH]\nDefaults to ~/.local. Installs this compiled executable, user-service asset and documentation. Does not enable/start.");
                return Ok(());
            }
            let mut prefix = None;
            let mut destdir = None;
            let mut iter = args.iter();
            while let Some(flag) = iter.next() {
                let value = iter.next().ok_or("installation option needs a path")?;
                match flag.as_str() {
                    "--prefix" if prefix.is_none() => prefix = Some(PathBuf::from(value)),
                    "--destdir" if destdir.is_none() => destdir = Some(PathBuf::from(value)),
                    _ => return Err(
                        "usage: ukvm install [--prefix ABSOLUTE_PATH] [--destdir ABSOLUTE_PATH]"
                            .into(),
                    ),
                }
            }
            let prefix = prefix.map_or_else(home_prefix, Ok)?;
            let binary = install(
                &std::env::current_exe().map_err(|e| e.to_string())?,
                &prefix,
                destdir.as_deref(),
            )?;
            println!("Installed {} plus lib/systemd/user/ukvm.service and share/doc/ukvm/SERVICE.md. No service state was changed.", binary.display());
            if destdir.is_none() {
                println!("Add {}/bin to PATH, then run ukvm service install. Use explicit service enable/start when ready.", prefix.display());
            }
            Ok(())
        })();
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => super::fail(&error),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_install_has_final_prefix_unit_without_starting_services() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("built");
        fs::write(&source, "compiled binary placeholder").unwrap();
        let binary = install(&source, Path::new("/usr"), Some(dir.path())).unwrap();
        assert_eq!(binary, dir.path().join("usr/bin/ukvm"));
        assert_eq!(
            fs::read_to_string(binary).unwrap(),
            "compiled binary placeholder"
        );
        let text =
            fs::read_to_string(dir.path().join("usr/lib/systemd/user/ukvm.service")).unwrap();
        assert!(text.contains("ExecStart=:\"/usr/bin/ukvm\" service run"));
        assert!(dir.path().join("usr/share/doc/ukvm/SERVICE.md").exists());
        assert!(dir
            .path()
            .join("usr/share/doc/ukvm/FRONTEND_CONTRACT.md")
            .exists());
        let schema: serde_json::Value = serde_json::from_slice(
            &fs::read(dir.path().join("usr/share/doc/ukvm/cli-schema-v1.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            schema["$defs"]["result"]["properties"]["schema_version"]["const"],
            1
        );
        assert!(stage(Path::new("/usr/../etc"), None).is_err());
        assert!(stage(Path::new("/usr"), Some(Path::new("relative"))).is_err());
    }
}
