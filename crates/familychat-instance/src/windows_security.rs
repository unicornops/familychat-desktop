//! The named pipe's security on Windows: the current user's SID, a
//! protected DACL that grants only that SID, and the pipe created with it
//! (PLAN.md, sections 5.1 and 5.5; ADR 0013).
//!
//! This is the only module in the workspace with `unsafe` code: the Win32
//! security calls have no safe wrapper in the dependency graph. Each block
//! says why it is sound. Everything it allocates is freed by a guard, and
//! no pointer outlives the call that made it.

#![allow(unsafe_code)]

use std::{io, ptr};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
};

/// Frees memory the security functions allocated with `LocalAlloc`.
struct LocalMemory(HLOCAL);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Win32 function documented to
            // allocate it with `LocalAlloc` and to be freed with
            // `LocalFree`, and this guard frees it once.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Closes a handle this module opened.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `OpenProcessToken` and is closed
        // once, here.
        unsafe { CloseHandle(self.0) };
    }
}

/// Reads a NUL-terminated UTF-16 string.
///
/// # Safety
///
/// `text` must point to a NUL-terminated UTF-16 string that stays valid
/// for the call.
unsafe fn wide_to_string(text: *const u16) -> String {
    let mut length = 0;
    // SAFETY: the caller guarantees a NUL terminator, so every read up to
    // and including it is inside the string.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` UTF-16 units starting at `text` were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
}

/// The current user's SID, as text (`S-1-5-21-…`).
pub(crate) fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
    // closing; `OpenProcessToken` writes a new handle to `token` only on
    // success.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);

    let mut needed = 0_u32;
    // SAFETY: a null buffer of length 0 asks only for the size, written to
    // `needed`; the call is expected to fail with "insufficient buffer".
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // `u64`s, so the buffer is aligned for `TOKEN_USER` (pointer-aligned).
    let words = usize::try_from(needed).unwrap_or(usize::MAX).div_ceil(8);
    let mut buffer = vec![0_u64; words];
    // SAFETY: the buffer is `needed` bytes or more, aligned to 8, and
    // writable; the function fills it with a `TOKEN_USER` followed by the
    // SID it points to.
    let filled = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so the buffer starts with a valid
    // `TOKEN_USER` whose SID pointer points inside the buffer, which lives
    // until the end of this function.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };

    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: the SID is valid (above); on success `text` receives a
    // `LocalAlloc`ed, NUL-terminated string, freed by the guard.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _free = LocalMemory(text.cast());
    // SAFETY: `ConvertSidToStringSidW` returned a NUL-terminated string,
    // which the guard keeps alive until after this read.
    Ok(unsafe { wide_to_string(text) })
}

/// The pipe's security descriptor: owned by the current user, with a
/// protected DACL whose one entry grants that user full access. Nobody
/// else (no group, not SYSTEM, not Administrators by DACL) can open it.
pub(crate) fn user_only_sddl() -> io::Result<String> {
    let sid = current_user_sid()?;
    if !sid.starts_with("S-1-") || !sid[4..].chars().all(|c| c.is_ascii_digit() || c == '-') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "an unexpected SID",
        ));
    }
    Ok(format!("O:{sid}D:P(A;;GA;;;{sid})"))
}

/// Creates the pipe's next instance `name` with `options` and the security
/// descriptor `sddl`. Must run inside the Tokio runtime.
pub(crate) fn create_pipe(
    options: &ServerOptions,
    name: &str,
    sddl: &str,
) -> io::Result<NamedPipeServer> {
    let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated and lives through the call; on
    // success `descriptor` receives a `LocalAlloc`ed self-relative
    // descriptor, freed by the guard after the pipe is made.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let _free = LocalMemory(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: `attributes` is a valid `SECURITY_ATTRIBUTES` whose
    // descriptor stays allocated until after the call, which copies it
    // into the new pipe; nothing keeps the pointer afterwards.
    unsafe {
        options.create_with_security_attributes_raw(name, ptr::from_mut(&mut attributes).cast())
    }
}

/// The owner and DACL of an open pipe, as SDDL (the permission test).
#[cfg(test)]
pub(crate) fn security_of(pipe: &NamedPipeServer) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Security::{
        Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
        },
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };

    let information = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is the open pipe's; the owner, group and ACL
    // out-pointers are optional and null; on success `descriptor` receives
    // a `LocalAlloc`ed descriptor, freed by the guard.
    let status = unsafe {
        GetSecurityInfo(
            pipe.as_raw_handle(),
            SE_KERNEL_OBJECT,
            information,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(-1),
        ));
    }
    let _free = LocalMemory(descriptor);
    let mut text: *mut u16 = ptr::null_mut();
    // SAFETY: the descriptor is valid (above); on success `text` receives a
    // `LocalAlloc`ed, NUL-terminated string, freed by the guard.
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            information,
            &mut text,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let _text = LocalMemory(text.cast());
    // SAFETY: a NUL-terminated string the guard keeps alive until after
    // this read.
    Ok(unsafe { wide_to_string(text) })
}
