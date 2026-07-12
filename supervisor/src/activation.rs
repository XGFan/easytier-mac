//! launchd socket activation (DESIGN §7).
//!
//! When started by launchd, the listening socket is created by launchd and
//! handed to us by name; we claim its fd(s) via `launch_activate_socket`
//! (launch.h, part of libSystem). Outside launchd this returns an error and the
//! caller must fall back to `--dev-listen` or exit.

use std::ffi::CString;
use std::io;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixListener;

unsafe extern "C" {
    // int launch_activate_socket(const char *name, int **fds, size_t *cnt);
    // Returns 0 on success; `fds` is a malloc'd array of length `cnt` that the
    // caller must free().
    fn launch_activate_socket(
        name: *const libc::c_char,
        fds: *mut *mut libc::c_int,
        cnt: *mut libc::size_t,
    ) -> libc::c_int;
}

/// Claim the launchd-provided listener sockets named `name` (our plist uses
/// the `Listeners` key, DESIGN §2).
pub fn activate_listeners(name: &str) -> io::Result<Vec<UnixListener>> {
    let cname = CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let mut fds: *mut libc::c_int = std::ptr::null_mut();
    let mut cnt: libc::size_t = 0;

    // SAFETY: FFI call fills `fds`/`cnt`; we only dereference them on success.
    let rc = unsafe { launch_activate_socket(cname.as_ptr(), &mut fds, &mut cnt) };
    if rc != 0 {
        // Non-zero is an errno (e.g. ESRCH when not launched by launchd).
        return Err(io::Error::from_raw_os_error(rc));
    }

    let mut listeners = Vec::with_capacity(cnt);
    // SAFETY: on success `fds` points to `cnt` ints; each is an owned fd we
    // adopt into a UnixListener. The array itself is freed with libc::free.
    unsafe {
        for i in 0..cnt {
            let fd = *fds.add(i);
            listeners.push(UnixListener::from_raw_fd(fd as RawFd));
        }
        if !fds.is_null() {
            libc::free(fds as *mut libc::c_void);
        }
    }

    if listeners.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "launchd returned no listener sockets",
        ));
    }
    Ok(listeners)
}
