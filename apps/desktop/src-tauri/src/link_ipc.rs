//! Where the browser link's pipe or socket lives, and (Windows) who may open it.
//!
//! Shared by the app (`link.rs`, the server) and the native-messaging relay (`relay.rs`, the
//! client): both must agree on the name without talking to each other first.

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use std::io;
    use std::ptr::null_mut;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// String form (`S-1-5-21-...`) of the user this process runs as. Cached: it cannot change.
    pub fn current_user_sid() -> io::Result<String> {
        static SID: OnceLock<String> = OnceLock::new();
        if let Some(s) = SID.get() {
            return Ok(s.clone());
        }
        let sid = query_sid()?;
        Ok(SID.get_or_init(|| sid).clone())
    }

    fn query_sid() -> io::Result<String> {
        // SAFETY: plain Win32 calls with buffers sized by the API itself; every handle and
        // allocation obtained here is released before returning.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = sid_of_token(token);
            CloseHandle(token);
            result
        }
    }

    unsafe fn sid_of_token(token: HANDLE) -> io::Result<String> {
        unsafe {
            let mut len = 0u32;
            // The first call only reports the size needed (it fails with ERROR_INSUFFICIENT_BUFFER).
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            if len == 0 {
                return Err(io::Error::last_os_error());
            }
            // u64 elements: TOKEN_USER holds pointers and needs 8-byte alignment.
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast::<c_void>(), len, &mut len) == 0 {
                return Err(io::Error::last_os_error());
            }
            let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
            let mut text: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut n = 0usize;
            while *text.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text.cast::<c_void>());
            Ok(s)
        }
    }

    /// A security descriptor that grants access to one user and nobody else, with the
    /// `SECURITY_ATTRIBUTES` that carry it into `CreateNamedPipe`.
    pub struct PipeSecurity {
        descriptor: *mut c_void,
        attrs: SECURITY_ATTRIBUTES,
    }

    impl PipeSecurity {
        /// `D:P(A;;GA;;;<sid>)`: a protected DACL (no inherited entries) with one allow entry,
        /// generic-all for the user.
        pub fn only_for(sid: &str) -> io::Result<Self> {
            let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut descriptor: *mut c_void = null_mut();
            // SAFETY: `sddl` is NUL-terminated and outlives the call; the descriptor it returns is
            // freed in `Drop`.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(PipeSecurity {
                descriptor,
                attrs: SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: descriptor,
                    bInheritHandle: 0,
                },
            })
        }

        /// Pointer for `ServerOptions::create_with_security_attributes_raw`; valid while `self` lives.
        pub fn as_ptr(&mut self) -> *mut c_void {
            std::ptr::from_mut(&mut self.attrs).cast::<c_void>()
        }
    }

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            if !self.descriptor.is_null() {
                // SAFETY: allocated by ConvertStringSecurityDescriptor... with LocalAlloc.
                unsafe {
                    LocalFree(self.descriptor);
                }
            }
        }
    }
}

#[cfg(windows)]
pub use win::{PipeSecurity, current_user_sid};

/// The pipe name for the current user (`\\.\pipe\ampsync-link-<hash of the SID>`).
#[cfg(windows)]
pub fn pipe_name() -> std::io::Result<String> {
    Ok(ampsync_core::browser_link::pipe_name_for(&current_user_sid()?))
}

/// The socket path for the current user (`$XDG_RUNTIME_DIR/ampsync-link.sock`, else in the data
/// directory).
#[cfg(unix)]
pub fn socket_path() -> std::path::PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from);
    ampsync_core::browser_link::socket_path(runtime.as_deref(), &crate::link_setup::data_dir())
}
