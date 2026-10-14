//! Owned Win32 security allocations and process-user identity. No secret values
//! or credential material are included in errors or Debug implementations.
use std::{
    ffi::c_void,
    io,
    mem::{size_of, MaybeUninit},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::{LocalFree, HANDLE},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        CopySid, GetLengthSid, GetTokenInformation, IsValidSid, TokenUser, TOKEN_QUERY, TOKEN_USER,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

pub(super) struct LocalMemory(pub(super) *mut c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns only allocations returned by Win32 APIs
        // documented to require LocalFree, never borrowed descriptor members.
        unsafe {
            LocalFree(self.0);
        }
    }
}

pub(super) fn process_token() -> io::Result<OwnedHandle> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: The process pseudo-handle is borrowed; the output token is owned
    // after success, non-inheritable, and closed exactly once by OwnedHandle.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

pub(super) struct User {
    sid: Vec<usize>,
    text: String,
}
impl User {
    pub(super) fn current() -> io::Result<Self> {
        let token = process_token()?;
        let mut length = 0;
        // SAFETY: The first call requests the bounded size only. The second
        // writes to an aligned allocation; TOKEN_USER and its SID are inspected
        // only after success, and the SID is copied before that buffer drops.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut length,
            );
        }
        if !(size_of::<TOKEN_USER>() as u32..=4096).contains(&length) {
            return Err(io::Error::other("invalid process user token size"));
        }
        let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        if unsafe { IsValidSid(user.User.Sid) } == 0 {
            return Err(io::Error::other("invalid process user SID"));
        }
        let bytes = unsafe { GetLengthSid(user.User.Sid) };
        if !(8..=68).contains(&bytes) {
            return Err(io::Error::other("invalid process user SID size"));
        }
        let mut sid = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        if unsafe { CopySid(bytes, sid.as_mut_ptr().cast(), user.User.Sid) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut text = ptr::null_mut();
        // SAFETY: The copied SID is valid and remains allocated. The returned
        // string is NUL terminated and belongs to the LocalMemory guard.
        if unsafe { ConvertSidToStringSidW(sid.as_ptr().cast_mut().cast(), &mut text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _allocation = LocalMemory(text.cast());
        let mut units = Vec::new();
        for index in 0..256 {
            let unit = unsafe { *text.add(index) };
            if unit == 0 {
                return Ok(Self {
                    sid,
                    text: String::from_utf16(&units)
                        .map_err(|_| io::Error::other("invalid SID encoding"))?,
                });
            }
            units.push(unit);
        }
        Err(io::Error::other("process SID exceeds size bound"))
    }
    pub(super) fn sid(&self) -> *mut c_void {
        self.sid.as_ptr().cast_mut().cast()
    }
    pub(super) fn descriptor(&self) -> io::Result<LocalMemory> {
        let sddl: Vec<u16> = format!("O:{}D:P(A;;FA;;;{})", self.text, self.text)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = ptr::null_mut();
        // SAFETY: NUL-terminated SDDL uses only the OS-returned SID. The returned
        // allocation is owned; the protected DACL grants full access only to it.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalMemory(descriptor))
    }
}

pub(super) fn elevation() -> io::Result<bool> {
    use windows_sys::Win32::Security::{TokenElevation, TOKEN_ELEVATION};
    let token = process_token()?;
    let mut value = MaybeUninit::<TOKEN_ELEVATION>::uninit();
    let mut length = 0;
    // SAFETY: The typed output has the API-required size and is read only after
    // success and confirmation of the returned byte length.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            value.as_mut_ptr().cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if length as usize != size_of::<TOKEN_ELEVATION>() {
        return Err(io::Error::other("invalid token elevation size"));
    }
    Ok(unsafe { value.assume_init() }.TokenIsElevated != 0)
}
