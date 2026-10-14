//! Read-only validation of the current non-elevated interactive Windows desktop.
use super::security::elevation;
use std::{io, mem::size_of, ptr};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{
        RemoteDesktop::{
            ProcessIdToSessionId, WTSActive, WTSConnectState, WTSFreeMemory,
            WTSQuerySessionInformationW,
        },
        StationsAndDesktops::{
            CloseDesktop, GetProcessWindowStation, GetThreadDesktop, GetUserObjectInformationW,
            OpenInputDesktop, DESKTOP_READOBJECTS, UOI_NAME,
        },
        Threading::{GetCurrentProcessId, GetCurrentThreadId},
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInfo {
    pub session_id: u32,
    pub active: bool,
    pub elevated: bool,
    pub interactive_desktop: bool,
}
impl SessionInfo {
    pub fn require_interactive(&self) -> io::Result<()> {
        if self.session_id == 0 || !self.active || self.elevated || !self.interactive_desktop {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Windows input requires a non-elevated active user session on the Default desktop",
            ));
        }
        Ok(())
    }
}
fn name(handle: HANDLE) -> io::Result<String> {
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = [0u16; 256];
    let mut bytes = 0;
    // SAFETY: The user-object handle is borrowed; output is bounded/aligned and
    // parsed only after success. A failure never requests a broader access mask.
    if unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            buffer.as_mut_ptr().cast(),
            size_of::<[u16; 256]>() as u32,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if bytes == 0 || bytes as usize > size_of::<[u16; 256]>() || bytes % 2 != 0 {
        return Err(io::Error::other("invalid desktop name size"));
    }
    let end = buffer
        .iter()
        .position(|v| *v == 0)
        .ok_or_else(|| io::Error::other("invalid desktop name"))?;
    String::from_utf16(&buffer[..end])
        .map_err(|_| io::Error::other("invalid desktop name encoding"))
}
pub fn probe() -> io::Result<SessionInfo> {
    let mut session_id = 0;
    // SAFETY: Only the current process/session is queried; these operations do
    // not select a desktop, set hooks, capture input or request OS grants.
    if unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session_id) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let elevated = elevation()?;
    let mut data = ptr::null_mut();
    let mut bytes = 0;
    if unsafe {
        WTSQuerySessionInformationW(
            ptr::null_mut(),
            session_id,
            WTSConnectState,
            &mut data,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    struct Buffer(*mut std::ffi::c_void);
    impl Drop for Buffer {
        fn drop(&mut self) {
            // SAFETY: WTS owns this allocation contract, distinct from LocalFree.
            unsafe { WTSFreeMemory(self.0) };
        }
    }
    let _data = Buffer(data.cast());
    if data.is_null() || bytes as usize != size_of::<i32>() {
        return Err(io::Error::other("invalid Windows session state size"));
    }
    let active = unsafe { ptr::read_unaligned(data.cast::<i32>()) } == WTSActive;
    let desktop = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
    let interactive_desktop = if desktop.is_null() {
        false
    } else {
        struct Desktop(windows_sys::Win32::System::StationsAndDesktops::HDESK);
        impl Drop for Desktop {
            fn drop(&mut self) {
                // SAFETY: This handle is owned; thread/process desktop handles are never closed here.
                unsafe { CloseDesktop(self.0) };
            }
        }
        let _desktop = Desktop(desktop);
        name(desktop).is_ok_and(|n| n.eq_ignore_ascii_case("Default"))
            && name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })
                .is_ok_and(|n| n.eq_ignore_ascii_case("Default"))
            && name(unsafe { GetProcessWindowStation() })
                .is_ok_and(|n| n.eq_ignore_ascii_case("WinSta0"))
    };
    Ok(SessionInfo {
        session_id,
        active,
        elevated,
        interactive_desktop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_zero_elevation_disconnect_and_secure_desktop_fail_closed() {
        let valid = SessionInfo {
            session_id: 1,
            active: true,
            elevated: false,
            interactive_desktop: true,
        };
        assert!(valid.require_interactive().is_ok());
        for invalid in [
            SessionInfo {
                session_id: 0,
                ..valid.clone()
            },
            SessionInfo {
                active: false,
                ..valid.clone()
            },
            SessionInfo {
                elevated: true,
                ..valid.clone()
            },
            SessionInfo {
                interactive_desktop: false,
                ..valid
            },
        ] {
            assert!(invalid.require_interactive().is_err());
        }
    }
}
