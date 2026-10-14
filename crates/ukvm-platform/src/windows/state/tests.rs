use super::*;

#[test]
fn private_creation_bounded_reads_and_atomic_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("private");
    assert!(StateDirectory::open(&path, false).is_err());
    assert!(!path.exists());
    let directory = StateDirectory::open(&path, true).unwrap();
    directory
        .write_new(OsStr::new("identity"), b"private fixture")
        .unwrap();
    assert!(directory
        .write_new(OsStr::new("identity"), b"overwrite")
        .is_err());
    assert!(directory.read(OsStr::new("identity"), 3).is_err());
    assert!(directory.read(OsStr::new("identity"), usize::MAX).is_err());
    assert_eq!(
        directory.read(OsStr::new("identity"), 32).unwrap(),
        b"private fixture"
    );
    directory
        .replace(OsStr::new("identity"), b"replacement")
        .unwrap();
    assert_eq!(
        directory.read(OsStr::new("identity"), 32).unwrap(),
        b"replacement"
    );
    assert_eq!(directory.entries().unwrap().count(), 1);
    directory.remove(OsStr::new("identity")).unwrap();
    assert!(directory.read(OsStr::new("identity"), 32).is_err());
    assert!(std::fs::rename(&path, temp.path().join("moved")).is_err());
    drop(directory);
    std::fs::rename(&path, temp.path().join("moved")).unwrap();
}

#[test]
fn permissive_existing_state_and_hardlinked_records_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("unsafe")).unwrap();
    assert!(StateDirectory::open(&temp.path().join("unsafe"), false).is_err());
    let path = temp.path().join("private");
    let directory = StateDirectory::open(&path, true).unwrap();
    directory
        .write_new(OsStr::new("record"), b"fixture")
        .unwrap();
    let external = temp.path().join("linked");
    std::fs::hard_link(path.join("record"), &external).unwrap();
    assert!(directory.read(OsStr::new("record"), 32).is_err());
    assert!(directory.replace(OsStr::new("record"), b"changed").is_err());
    assert_eq!(std::fs::read(external).unwrap(), b"fixture");
}

#[test]
fn additional_acl_grants_are_rejected_without_repair() {
    use windows_sys::Win32::Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SetSecurityInfo},
        GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    let temp = tempfile::tempdir().unwrap();
    let directory = StateDirectory::open(&temp.path().join("private"), true).unwrap();
    directory
        .write_new(OsStr::new("record"), b"fixture")
        .unwrap();
    let file = directory
        .file(OsStr::new("record"), WRITE_DAC, OPEN_EXISTING, true)
        .unwrap();
    let sddl: Vec<u16> = "D:P(A;;FA;;;WD)".encode_utf16().chain(Some(0)).collect();
    let mut descriptor = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: This test changes only an owned isolated fixture. The descriptor
    // and its ACL remain live until SetSecurityInfo has copied them.
    unsafe {
        assert_ne!(
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut()
            ),
            0
        );
        let _descriptor = LocalMemory(descriptor);
        assert_ne!(
            GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted),
            0
        );
        assert_eq!(
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null_mut()
            ),
            0
        );
    }
    assert!(private(&file, &directory.user).is_err());
    assert!(directory.read(OsStr::new("record"), 32).is_err());
    assert!(directory.replace(OsStr::new("record"), b"changed").is_err());
    // Restore fixture permissions for cleanup, never through the production API.
    let descriptor = directory.user.descriptor().unwrap();
    unsafe {
        assert_ne!(
            GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted),
            0
        );
        assert_eq!(
            SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null_mut()
            ),
            0
        );
    }
}

#[test]
fn unsafe_names_and_nonlocal_paths_are_rejected() {
    for name in [
        "..",
        "CON",
        "COM1.txt",
        "COM¹.txt",
        "CON .txt",
        "CLOCK$",
        "LPT9",
        "record:stream",
        "trailing.",
        "trailing ",
        "sub/record",
        "sub\\record",
        "nul\0",
    ] {
        assert!(component(OsStr::new(name)).is_err(), "{name:?}");
    }
    for path in [
        "relative",
        "C:relative",
        "\\\\server\\share\\ukvm",
        "\\\\?\\C:\\ukvm",
        "\\\\.\\pipe\\ukvm",
    ] {
        assert!(StateDirectory::open(Path::new(path), false).is_err());
    }
}

#[test]
fn a_crash_between_invalidation_and_delete_cannot_expose_old_state() {
    let temp = tempfile::tempdir().unwrap();
    let directory = StateDirectory::open(&temp.path().join("private"), true).unwrap();
    directory
        .write_new(
            OsStr::new("receipt"),
            b"trusted fixture data that must be invalidated",
        )
        .unwrap();
    directory.invalidate(OsStr::new("receipt")).unwrap();
    assert_eq!(
        directory.read(OsStr::new("receipt"), 64).unwrap(),
        REVOKED_RECORD.as_bytes()
    );
    directory.revoke_and_remove(OsStr::new("receipt")).unwrap();
    assert!(directory.read(OsStr::new("receipt"), 64).is_err());
}

#[test]
#[ignore = "requires native Windows symbolic-link privilege or Developer Mode"]
fn ancestor_and_record_reparse_points_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("private");
    let directory = StateDirectory::open(&path, true).unwrap();
    let external = temp.path().join("external");
    std::fs::write(&external, b"fixture").unwrap();
    std::os::windows::fs::symlink_file(&external, path.join("record")).unwrap();
    assert!(directory.read(OsStr::new("record"), 32).is_err());
    assert!(directory.replace(OsStr::new("record"), b"changed").is_err());
    std::os::windows::fs::symlink_dir(&path, temp.path().join("redirect")).unwrap();
    assert!(StateDirectory::open(&temp.path().join("redirect/child"), true).is_err());
    assert_eq!(std::fs::read(external).unwrap(), b"fixture");
    assert!(!path.join("child").exists());
}
