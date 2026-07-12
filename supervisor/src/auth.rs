//! Peer credential authorization (DESIGN §6).
//!
//! On connect we read the peer's uid via `LOCAL_PEERCRED` and gate it. In
//! launchd mode we allow `{0, owner_uid}`; in `--dev-listen` mode we degrade to
//! "same euid as this process" so non-root integration tests and GUI bring-up
//! work without install.
//!
//! Code-signature pinning (SecCode / Team ID) is M2; the `signed-peers`
//! feature reserves the seam with a no-op.

use std::io;
use std::os::unix::io::RawFd;

/// Read the connected peer's effective uid from a unix-domain socket.
///
/// Uses `getsockopt(SOL_LOCAL, LOCAL_PEERCRED)` which fills a `struct xucred`
/// (sys/ucred.h). `cr_uid` is the peer's effective uid at connect time.
pub fn peer_uid(fd: RawFd) -> io::Result<u32> {
    // SAFETY: `cred` is a POD struct we hand to getsockopt with its byte length;
    // the kernel fills it and reports the written length back through `len`.
    unsafe {
        let mut cred: libc::xucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::xucred>() as libc::socklen_t;
        let rc = libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERCRED,
            &mut cred as *mut libc::xucred as *mut libc::c_void,
            &mut len,
        );
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // Fail-closed hardening: the kernel must have written a full xucred, at
        // the expected version, with at least the primary group present. A short
        // or zeroed-out struct means we cannot trust cr_uid.
        if len as usize != std::mem::size_of::<libc::xucred>() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected xucred size {len}"),
            ));
        }
        if cred.cr_version != libc::XUCRED_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected xucred version {}", cred.cr_version),
            ));
        }
        if cred.cr_ngroups < 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "xucred reported no groups",
            ));
        }
        Ok(cred.cr_uid)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AuthMode {
    /// launchd/production: allow root and the installing user.
    Launchd { owner_uid: u32 },
    /// `--dev-listen`: allow only the process's own euid.
    Dev { euid: u32 },
}

impl AuthMode {
    pub fn allows(&self, uid: u32) -> bool {
        match self {
            AuthMode::Launchd { owner_uid } => uid == 0 || uid == *owner_uid,
            AuthMode::Dev { euid } => uid == *euid,
        }
    }
}

/// M2 seam: verify the peer's code signature / pinned Team ID. No-op for now.
#[cfg(feature = "signed-peers")]
#[allow(dead_code)] // reserved for M2; not wired into the connection path yet.
pub fn verify_signed_peer(_fd: RawFd) -> io::Result<()> {
    // TODO(M2): resolve SecCodeRef from the peer audit token and pin Team ID.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_allows_root_and_owner_only() {
        let m = AuthMode::Launchd { owner_uid: 501 };
        assert!(m.allows(0));
        assert!(m.allows(501));
        assert!(!m.allows(502));
    }

    #[test]
    fn dev_allows_only_self() {
        let m = AuthMode::Dev { euid: 501 };
        assert!(m.allows(501));
        assert!(!m.allows(0));
        assert!(!m.allows(502));
    }
}
