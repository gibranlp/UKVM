//! Private local Windows state. Handles pin every ancestor against replacement;
//! final objects must have a protected, current-user-only DACL. Reparse points,
//! extra hard links, device/UNC paths, ADS and non-ACL filesystems fail closed.
use super::security::{LocalMemory, User};
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read, Write},
    mem::{size_of, MaybeUninit},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr,
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
    Security::{
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT},
        EqualSid, GetAce, GetSecurityDescriptorControl, IsValidAcl, IsValidSid, ACCESS_ALLOWED_ACE,
        ACE_HEADER, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
        SE_DACL_PROTECTED,
    },
    Storage::FileSystem::*,
    System::{
        SystemServices::{ACCESS_ALLOWED_ACE_TYPE, FILE_PERSISTENT_ACLS},
        WindowsProgramming::DRIVE_FIXED,
    },
};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn component(name: &OsStr) -> io::Result<()> {
    let name = name
        .to_str()
        .ok_or_else(|| invalid("invalid Windows state name"))?;
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        || matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
        )
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
    {
        return Err(invalid("unsafe Windows state name"));
    }
    Ok(())
}

fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = MaybeUninit::uninit();
    // SAFETY: The live File owns the handle; the output is initialized and read
    // only after GetFileInformationByHandle succeeds.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.assume_init() })
}
fn regular(file: &File, directory: bool) -> io::Result<()> {
    let info = information(file)?;
    // SAFETY: GetFileType borrows the live File handle without transferring it.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
        || (!directory && info.nNumberOfLinks != 1)
    {
        return Err(invalid(
            "Windows state must use ordinary objects without reparse points or extra hard links",
        ));
    }
    Ok(())
}
fn private(file: &File, user: &User) -> io::Result<()> {
    let mut owner = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: The API allocates one descriptor, with owner/DACL pointers into it.
    // Those pointers remain borrowed until the LocalMemory guard is dropped.
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _descriptor = LocalMemory(descriptor);
    let mut control = 0;
    let mut revision = 0;
    if descriptor.is_null()
        || owner.is_null()
        || dacl.is_null()
        || unsafe { EqualSid(owner, user.sid()) } == 0
        || unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
        || unsafe { IsValidAcl(dacl) } == 0
        || unsafe { (*dacl).AceCount } != 1
    {
        return Err(invalid("Windows state requires a protected DACL owned by the current user; existing unsafe state is not repaired automatically"));
    }
    let mut ace = ptr::null_mut();
    // SAFETY: The ACL is OS-validated, has one ACE, and its allocation remains
    // live. Read the header first and validate size/type before the allow ACE.
    if unsafe { GetAce(dacl, 0, &mut ace) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    if header.AceType as u32 != ACCESS_ALLOWED_ACE_TYPE
        || header.AceFlags != 0
        || (header.AceSize as usize) < std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart) + 8
    {
        return Err(invalid(
            "Windows state DACL contains an unexpected access entry",
        ));
    }
    let allow = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    let sid_bytes = ptr::addr_of!(allow.SidStart).cast::<u8>();
    // SAFETY: The fixed SID header fits inside the validated ACE. Check its
    // variable subauthority count against the ACE extent before SID APIs read it.
    let subauthorities = unsafe { *sid_bytes.add(1) } as usize;
    if subauthorities > 15
        || 8 + 4 * subauthorities
            > header.AceSize as usize - std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart)
    {
        return Err(invalid(
            "Windows state access entry has an invalid SID extent",
        ));
    }
    let sid = sid_bytes.cast_mut().cast();
    if allow.Mask != FILE_ALL_ACCESS
        || unsafe { IsValidSid(sid) } == 0
        || unsafe { EqualSid(sid, user.sid()) } == 0
    {
        return Err(invalid(
            "Windows state grants access beyond the current user",
        ));
    }
    Ok(())
}
fn open(
    path: &Path,
    access: u32,
    disposition: u32,
    security: *const SECURITY_ATTRIBUTES,
    share_delete: bool,
) -> io::Result<File> {
    let path = wide(path);
    // SAFETY: Path and optional security descriptor are live for this call.
    // Reparse points are opened themselves, then rejected through handle data.
    // Handles are non-inheritable and transferred exactly once to File.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | if share_delete { FILE_SHARE_DELETE } else { 0 },
            security,
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

pub struct StateDirectory {
    path: PathBuf,
    _ancestors: Vec<File>,
    user: User,
}
/// A flushed marker invalidates removed security state even if a crash leaves
/// its directory entry behind. It contains no trust, identity or permissions.
pub const REVOKED_RECORD: &str = ">>UKVM-REVOKED<<\n";
impl StateDirectory {
    pub fn open(path: &Path, create: bool) -> io::Result<Self> {
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return Err(invalid(
                "Windows state directory must be an absolute local drive path",
            ));
        };
        if !matches!(prefix.kind(), Prefix::Disk(_))
            || components.next() != Some(Component::RootDir)
        {
            return Err(invalid(
                "Windows state directory must be an ordinary absolute local drive path",
            ));
        }
        let names: Vec<_> = components
            .map(|part| {
                let Component::Normal(name) = part else {
                    return Err(invalid("Windows state path cannot contain traversal"));
                };
                component(name)?;
                Ok(name)
            })
            .collect::<io::Result<_>>()?;
        if names.is_empty() || names.len() > 128 || path.as_os_str().encode_wide().count() > 32760 {
            return Err(invalid(
                "Windows state directory exceeds path bounds or is a drive root",
            ));
        }
        let user = User::current()?;
        let descriptor = user.descriptor()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let mut current = PathBuf::from(prefix.as_os_str());
        current.push(Path::new("\\"));
        let mut ancestors = Vec::new();
        // SAFETY: GetDriveType borrows a NUL-terminated root. Network/removable
        // drives are outside the private per-user state contract.
        if unsafe { GetDriveTypeW(wide(&current).as_ptr()) } != DRIVE_FIXED {
            return Err(invalid("Windows state requires a local fixed drive"));
        }
        let root = open(
            &current,
            FILE_READ_ATTRIBUTES,
            OPEN_EXISTING,
            ptr::null(),
            false,
        )?;
        regular(&root, true)?;
        ancestors.push(root);
        let mut count = 0;
        for name in names {
            current.push(name);
            count += 1;
            if create {
                // SAFETY: Every parent is already pinned with a no-delete-share
                // handle. New directories receive the protected DACL at creation.
                if unsafe { CreateDirectoryW(wide(&current).as_ptr(), &attributes) } == 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(error);
                    }
                }
            }
            let handle = open(
                &current,
                READ_CONTROL | FILE_READ_ATTRIBUTES,
                OPEN_EXISTING,
                ptr::null(),
                false,
            )?;
            regular(&handle, true)?;
            ancestors.push(handle);
        }
        if count == 0 {
            return Err(invalid("a drive root cannot be UKVM private state"));
        }
        let handle = ancestors.last().unwrap();
        let mut flags = 0;
        let mut filesystem = [0u16; 32];
        // SAFETY: The live directory identifies the volume. Outputs have the
        // declared sizes and no volume-name or serial output is requested.
        if unsafe {
            GetVolumeInformationByHandleW(
                handle.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut flags,
                filesystem.as_mut_ptr(),
                filesystem.len() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let end = filesystem
            .iter()
            .position(|v| *v == 0)
            .unwrap_or(filesystem.len());
        let filesystem = String::from_utf16_lossy(&filesystem[..end]);
        if flags & FILE_PERSISTENT_ACLS == 0 || !matches!(filesystem.as_str(), "NTFS" | "ReFS") {
            return Err(invalid("private Windows state requires NTFS or ReFS ACLs"));
        }
        private(handle, &user)?;
        Ok(Self {
            path: current,
            _ancestors: ancestors,
            user,
        })
    }
    fn path(&self, name: &OsStr) -> io::Result<PathBuf> {
        component(name)?;
        Ok(self.path.join(name))
    }
    fn file(
        &self,
        name: &OsStr,
        access: u32,
        disposition: u32,
        share_delete: bool,
    ) -> io::Result<File> {
        let descriptor = self.user.descriptor()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let path = self.path(name)?;
        let existing = || {
            open(
                &path,
                access | READ_CONTROL,
                OPEN_EXISTING,
                ptr::null(),
                share_delete,
            )
        };
        // Supply creation security only for CREATE_NEW. Existing objects are
        // opened without a descriptor and must pass validation unchanged.
        let file = match disposition {
            OPEN_EXISTING => existing()?,
            CREATE_NEW => open(
                &path,
                access | READ_CONTROL,
                CREATE_NEW,
                &attributes,
                share_delete,
            )?,
            OPEN_ALWAYS => match existing() {
                Ok(file) => file,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    match open(
                        &path,
                        access | READ_CONTROL,
                        CREATE_NEW,
                        &attributes,
                        share_delete,
                    ) {
                        Ok(file) => file,
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => existing()?,
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => return Err(e),
            },
            _ => return Err(invalid("unsupported Windows state open mode")),
        };
        regular(&file, false)?;
        private(&file, &self.user)?;
        Ok(file)
    }
    pub fn lock_file(&self, name: &OsStr, create: bool) -> io::Result<File> {
        self.file(
            name,
            GENERIC_READ | GENERIC_WRITE,
            if create { OPEN_ALWAYS } else { OPEN_EXISTING },
            false,
        )
    }
    pub fn read(&self, name: &OsStr, maximum: usize) -> io::Result<Vec<u8>> {
        if maximum > 1024 * 1024 {
            return Err(invalid("Windows state read exceeds maximum record bound"));
        }
        let file = self.file(name, GENERIC_READ, OPEN_EXISTING, true)?;
        let mut bytes = Vec::new();
        file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > maximum {
            return Err(invalid("Windows state record exceeds size bound"));
        }
        Ok(bytes)
    }
    pub fn write_new(&self, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        let mut file = self.file(name, GENERIC_WRITE, CREATE_NEW, true)?;
        file.write_all(bytes)?;
        file.sync_all()
    }
    pub fn replace(&self, name: &OsStr, bytes: &[u8]) -> io::Result<()> {
        let destination = self.path(name)?;
        match self.file(name, FILE_READ_ATTRIBUTES, OPEN_EXISTING, true) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let temp = format!(".ukvm-{}-{stamp}.pending", std::process::id());
        if let Err(error) = self.write_new(OsStr::new(&temp), bytes) {
            let _ = self.remove(OsStr::new(&temp));
            return Err(error);
        }
        let source = self.path(OsStr::new(&temp))?;
        // SAFETY: Both paths are children of the pinned private directory; the
        // temp has been flushed/closed. No cross-volume copy fallback is allowed.
        if unsafe {
            MoveFileExW(
                wide(&source).as_ptr(),
                wide(&destination).as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            let _ = self.remove(OsStr::new(&temp));
            return Err(error);
        }
        Ok(())
    }
    pub fn remove(&self, name: &OsStr) -> io::Result<()> {
        let file = self.file(name, DELETE, OPEN_EXISTING, true)?;
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: Delete is applied to the validated file handle, not to a
        // possibly replaced path. The File closes once, completing deletion.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                ptr::addr_of!(disposition).cast(),
                size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn invalidate(&self, name: &OsStr) -> io::Result<()> {
        let mut file = self.file(name, GENERIC_WRITE, OPEN_EXISTING, true)?;
        file.write_all(REVOKED_RECORD.as_bytes())?;
        file.set_len(REVOKED_RECORD.len() as u64)?;
        file.sync_all()
    }
    pub fn revoke_and_remove(&self, name: &OsStr) -> io::Result<()> {
        // Flush invalidation in place before unlinking. A rolled-back/uncompleted
        // deletion cannot expose a still-valid pairing recovery receipt.
        self.invalidate(name)?;
        self.remove(name)
    }
    pub fn entries(&self) -> io::Result<std::fs::ReadDir> {
        std::fs::read_dir(&self.path)
    }
}

pub fn read(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("state record has no parent"))?;
    StateDirectory::open(parent, false)?.read(
        path.file_name()
            .ok_or_else(|| invalid("state record has no filename"))?,
        maximum,
    )
}

pub fn runtime_directory() -> io::Result<PathBuf> {
    dirs::data_local_dir()
        .map(|base| base.join("ukvm/runtime"))
        .ok_or_else(|| invalid("unable to locate private Windows runtime state directory"))
}
pub fn read_text(path: &Path, maximum: usize) -> io::Result<String> {
    String::from_utf8(read(path, maximum)?)
        .map_err(|_| invalid("Windows state record is not UTF-8"))
}
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    StateDirectory::open(
        path.parent()
            .ok_or_else(|| invalid("state record has no parent"))?,
        true,
    )?
    .replace(
        path.file_name()
            .ok_or_else(|| invalid("state record has no filename"))?,
        bytes,
    )
}
pub fn remove(path: &Path) -> io::Result<()> {
    StateDirectory::open(
        path.parent()
            .ok_or_else(|| invalid("state record has no parent"))?,
        false,
    )?
    .remove(
        path.file_name()
            .ok_or_else(|| invalid("state record has no filename"))?,
    )
}

pub fn revoke_and_remove(path: &Path) -> io::Result<()> {
    StateDirectory::open(
        path.parent()
            .ok_or_else(|| invalid("state record has no parent"))?,
        false,
    )?
    .revoke_and_remove(
        path.file_name()
            .ok_or_else(|| invalid("state record has no filename"))?,
    )
}

#[cfg(test)]
mod tests;
