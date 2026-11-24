//! Where the control socket lives.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{ControlError, Result};

/// The environment variable that names the control socket. The app sets it for every pane it spawns.
pub const CONTROL_SOCKET_ENV: &str = "XCTX_CONTROL_SOCKET";

/// The socket's file name inside the data directory when `XCTX_CONTROL_SOCKET` is not set.
pub const SOCKET_FILE_NAME: &str = "control.sock";

/// The longest socket path, in bytes, a Unix socket address holds on macOS (`sun_path` is 104 bytes including
/// the terminating NUL).
pub const SUN_PATH_MAX: usize = 103;

/// The control socket path: `$XCTX_CONTROL_SOCKET` when set and non-empty, else `control.sock` in
/// [`xctx_core::store::data_dir`].
pub fn socket_path() -> Result<PathBuf> {
    resolve_socket_path(
        std::env::var_os(CONTROL_SOCKET_ENV),
        xctx_core::store::data_dir,
    )
}

/// [`socket_path`] from an explicit value of `XCTX_CONTROL_SOCKET` and a way to find the data directory, which
/// is only asked for when the variable is unset or empty.
pub fn resolve_socket_path(
    control_socket: Option<OsString>,
    data_dir: impl FnOnce() -> xctx_core::Result<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = control_socket.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(data_dir()?.join(SOCKET_FILE_NAME))
}

/// Fail with [`ControlError::SocketPathTooLong`] when `path` does not fit in a Unix socket address, so the error
/// names the problem instead of a bare `EINVAL`.
pub fn check_socket_path(path: &Path) -> Result<()> {
    let len = path.as_os_str().len();
    if len > SUN_PATH_MAX {
        return Err(ControlError::SocketPathTooLong {
            path: path.to_path_buf(),
            len,
            max: SUN_PATH_MAX,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_env_var_wins_and_the_data_dir_is_the_default() -> Result<()> {
        let explicit = resolve_socket_path(Some("/tmp/x.sock".into()), || {
            Err(xctx_core::Error::NoDataDir)
        })?;
        assert_eq!(explicit, PathBuf::from("/tmp/x.sock"));
        let default = resolve_socket_path(Some("".into()), || Ok(PathBuf::from("/data")))?;
        assert_eq!(default, PathBuf::from("/data/control.sock"));
        assert!(matches!(
            resolve_socket_path(None, || Err(xctx_core::Error::NoDataDir)),
            Err(ControlError::NoSocketPath(_))
        ));
        Ok(())
    }

    #[test]
    fn long_paths_are_rejected_with_their_length() {
        let long = PathBuf::from(format!("/{}", "a".repeat(SUN_PATH_MAX)));
        assert!(matches!(
            check_socket_path(&long),
            Err(ControlError::SocketPathTooLong { len, .. }) if len == SUN_PATH_MAX + 1
        ));
        assert!(check_socket_path(Path::new("/tmp/control.sock")).is_ok());
    }
}
