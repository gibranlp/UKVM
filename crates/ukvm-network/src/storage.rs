//! Bounded user-state operations. Windows native ACL/path handling lives in
//! ukvm-platform; portable parsing and policy stay in this crate.
#[cfg(not(target_os = "windows"))]
use std::{fs, io::Read};
use std::{io, path::Path};

pub(crate) fn directory(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        ukvm_platform::windows::state::StateDirectory::open(path, true).map(|_| ())
    }
    #[cfg(not(target_os = "windows"))]
    {
        fs::create_dir_all(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}
pub(crate) fn read(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    #[cfg(target_os = "windows")]
    {
        ukvm_platform::windows::state::read(path, maximum)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(maximum as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "user-state record exceeds size bound",
            ));
        }
        Ok(bytes)
    }
}
pub(crate) fn text(path: &Path, maximum: usize) -> io::Result<String> {
    String::from_utf8(read(path, maximum)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "user-state record is not UTF-8"))
}
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        ukvm_platform::windows::state::revoke_and_remove(path)
    }
    #[cfg(not(target_os = "windows"))]
    {
        fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_identity_trust_and_recovery_reads_do_not_return_oversized_records() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("private");
        directory(&dir).unwrap();
        let path = dir.join("record");
        crate::trust::atomic_write(&path, b"fixture").unwrap();
        assert!(read(&path, 3).is_err());
        assert_eq!(read(&path, 7).unwrap(), b"fixture");
        crate::trust::atomic_write(&path, &[0xff]).unwrap();
        assert!(text(&path, 8).is_err());
    }
}
