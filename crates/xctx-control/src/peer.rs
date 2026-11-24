//! Who is on the other end of a Unix socket, checked by both the server (its clients) and the client (its server).

use std::io;
use std::os::unix::net::UnixStream;

/// Whether the process on the other end of `stream` runs as the same user as this one.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
pub(crate) fn peer_is_same_user(stream: &UnixStream) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the fd is a connected Unix socket owned by `stream` for the duration of the call, and both out
    // pointers point at live locals of the right type.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    Ok(uid == me)
}

/// Whether the process on the other end of `stream` runs as the same user as this one. Without `getpeereid` the
/// socket's owner-only mode is the only check.
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
)))]
pub(crate) fn peer_is_same_user(_stream: &UnixStream) -> io::Result<bool> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_pair_in_this_process_is_the_same_user() -> io::Result<()> {
        let (a, b) = UnixStream::pair()?;
        assert!(peer_is_same_user(&a)?);
        assert!(peer_is_same_user(&b)?);
        Ok(())
    }
}
