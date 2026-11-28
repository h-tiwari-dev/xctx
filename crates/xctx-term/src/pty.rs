//! Opening a pseudo-terminal and starting a program on it.

use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use crate::config::TerminalConfig;
use crate::error::TermError;

/// The controlling side of a PTY with a program running on the other side.
pub(crate) struct SpawnedPty {
    /// The PTY master. Reads return program output; writes are program input.
    pub master: File,
    /// The program. It leads its own session and process group.
    pub child: Child,
    /// The parent's copy of the PTY slave. Holding it keeps the PTY from hanging up if the
    /// program exits at once; drop it after registering `master` for readiness, so reading the
    /// master reports end-of-file once the program and its children have exited.
    pub slave: OwnedFd,
}

fn winsize(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols.saturating_mul(cell_width),
        ws_ypixel: rows.saturating_mul(cell_height),
    }
}

/// `TIOCPTYGNAME` from `<sys/ttycom.h>`: `_IOR('t', 83, char[128])`. Writes the slave's path
/// into a caller-owned buffer, unlike `ptsname(3)`, which returns a shared static buffer.
const TIOCPTYGNAME: libc::c_ulong = 0x4080_7453;

/// Size of the buffer `TIOCPTYGNAME` fills.
const PTY_NAME_SIZE: usize = 128;

/// Attempts at opening `/dev/ptmx` before giving up on a transient failure.
const PTMX_OPEN_ATTEMPTS: u32 = 16;

fn open_cloexec(path: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: open succeeded, so `fd` is open and owned by us alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens a new PTY master.
///
/// macOS fails `open("/dev/ptmx")` now and then with ENXIO (reported as -ENXIO) while other
/// threads open and close PTYs; trying again gets a working master.
fn open_ptmx() -> io::Result<OwnedFd> {
    let mut attempt = 1;
    loop {
        match open_cloexec(c"/dev/ptmx") {
            Err(err)
                if attempt < PTMX_OPEN_ATTEMPTS
                    && matches!(err.raw_os_error(), Some(code) if code.abs() == libc::ENXIO) =>
            {
                attempt += 1;
                std::thread::yield_now();
            }
            result => return result,
        }
    }
}

/// Opens a PTY pair sized to `cols` x `rows`, returning (master, slave).
///
/// Both ends are opened with `O_CLOEXEC`, so a program started concurrently on another thread
/// never inherits them, and the slave is found with `TIOCPTYGNAME` rather than the
/// thread-unsafe `ptsname(3)` that `openpty(3)` uses.
fn open_pty(
    cols: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
) -> Result<(OwnedFd, OwnedFd), TermError> {
    let master = open_ptmx().map_err(TermError::OpenPty)?;
    // SAFETY: grantpt and unlockpt take a descriptor we own.
    if unsafe { libc::grantpt(master.as_raw_fd()) } == -1 {
        return Err(TermError::OpenPty(io::Error::last_os_error()));
    }
    // SAFETY: as above.
    if unsafe { libc::unlockpt(master.as_raw_fd()) } == -1 {
        return Err(TermError::OpenPty(io::Error::last_os_error()));
    }
    let mut name = [0 as libc::c_char; PTY_NAME_SIZE];
    // SAFETY: TIOCPTYGNAME writes at most PTY_NAME_SIZE bytes into `name`, which outlives the
    // call.
    if unsafe { libc::ioctl(master.as_raw_fd(), TIOCPTYGNAME, name.as_mut_ptr()) } == -1 {
        return Err(TermError::OpenPty(io::Error::last_os_error()));
    }
    // Guarantee termination even if the kernel filled the whole buffer.
    name[PTY_NAME_SIZE - 1] = 0;
    // SAFETY: `name` is NUL-terminated (ensured above) and lives for this scope.
    let slave_path = unsafe { CStr::from_ptr(name.as_ptr()) };
    let slave = open_cloexec(slave_path).map_err(TermError::OpenPty)?;

    let size = winsize(cols, rows, cell_width, cell_height);
    // SAFETY: TIOCSWINSZ reads a winsize struct that lives for the duration of the call.
    if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) } == -1 {
        return Err(TermError::ConfigurePty(io::Error::last_os_error()));
    }

    // Tell the line discipline input is UTF-8 so backspace in canonical mode erases whole
    // characters.
    // SAFETY: termios is plain data filled in by tcgetattr before use.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(slave.as_raw_fd(), &mut termios) == 0 {
            termios.c_iflag |= libc::IUTF8;
            libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &termios);
        }
    }
    Ok((master, slave))
}

/// Starts `config.program` on a new PTY.
pub(crate) fn spawn(config: &TerminalConfig) -> Result<SpawnedPty, TermError> {
    let cols = config.cols.max(2);
    let rows = config.rows.max(1);
    let (master, slave) = open_pty(cols, rows, config.cell_width, config.cell_height)?;

    let stdio = |slave: &OwnedFd| -> Result<Stdio, TermError> {
        Ok(Stdio::from(
            slave.try_clone().map_err(TermError::ConfigurePty)?,
        ))
    };

    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .envs(&config.env)
        .stdin(stdio(&slave)?)
        .stdout(stdio(&slave)?)
        .stderr(stdio(&slave)?);
    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }

    // SAFETY: the closure only calls async-signal-safe functions (setsid, ioctl, signal).
    unsafe {
        command.pre_exec(|| {
            // A new session makes the PTY the controlling terminal, so job control works and
            // the program gets SIGHUP when the terminal goes away.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // stdin is the PTY slave by the time pre_exec runs.
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            for signal in [
                libc::SIGCHLD,
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
                libc::SIGPIPE,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }

    let child = command.spawn().map_err(|source| TermError::Spawn {
        program: config.program.clone(),
        source,
    })?;
    // Close the Command's copies of the slave; the caller closes `slave` once it is ready.
    drop(command);

    Ok(SpawnedPty {
        master: File::from(master),
        child,
        slave,
    })
}

/// Tells the kernel (and through SIGWINCH, the program) the new window size.
pub(crate) fn set_window_size(
    master: &File,
    cols: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
) -> io::Result<()> {
    let size = winsize(cols, rows, cell_width, cell_height);
    // SAFETY: TIOCSWINSZ reads a winsize struct that lives for the duration of the call.
    let result = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sends `signal` to the process group led by `pid`, falling back to the process itself.
pub(crate) fn signal_group(pid: u32, signal: libc::c_int) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: kill takes plain integers. The caller guarantees `pid` has not been reaped, so
    // it cannot name an unrelated process.
    unsafe {
        if libc::kill(-pid, signal) == -1 {
            libc::kill(pid, signal);
        }
    }
}

/// The PTY's foreground process group, if the program's session still owns the terminal.
pub(crate) fn foreground_group(master: &File) -> Option<libc::pid_t> {
    // SAFETY: tcgetpgrp takes a descriptor that `master` keeps open.
    let group = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    (group > 0).then_some(group)
}

/// Sends `signal` to every process in `group`. The caller must have seen `group` in use
/// recently (for example from [`foreground_group`]).
pub(crate) fn signal_process_group(group: libc::pid_t, signal: libc::c_int) {
    if group > 0 {
        // SAFETY: kill takes plain integers; a negative pid names a process group.
        unsafe {
            libc::kill(-group, signal);
        }
    }
}

/// Whether any process (including a zombie) is still in `group`.
pub(crate) fn process_group_exists(group: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks that the group exists and may be signalled.
    group > 0 && unsafe { libc::kill(-group, 0) } == 0
}

/// The live working directory of the foreground process (or `fallback_pid`), on macOS.
pub(crate) fn working_directory(master: &File, fallback_pid: u32) -> Option<std::path::PathBuf> {
    if let Some(group) = foreground_group(master)
        && let Some(dir) = pid_working_directory(group)
    {
        return Some(dir);
    }
    let pid = libc::pid_t::try_from(fallback_pid).ok()?;
    pid_working_directory(pid)
}

#[cfg(target_os = "macos")]
fn pid_working_directory(pid: libc::pid_t) -> Option<std::path::PathBuf> {
    if pid <= 0 {
        return None;
    }
    // SAFETY: `proc_vnodepathinfo` is plain C data zero-initialized before passing to `proc_pidinfo`.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            std::ptr::from_mut(&mut info).cast::<libc::c_void>(),
            size,
        )
    };
    if written <= 0 {
        return None;
    }
    // SAFETY: `vip_path` is a NUL-terminated fixed-size C buffer (`[[c_char; 32]; 32]`, 1024 bytes) filled by the kernel.
    let bytes = unsafe {
        CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast::<libc::c_char>()).to_bytes()
    };
    if bytes.is_empty() {
        return None;
    }
    use std::os::unix::ffi::OsStrExt;
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
}

#[cfg(not(target_os = "macos"))]
fn pid_working_directory(_pid: libc::pid_t) -> Option<std::path::PathBuf> {
    None
}

/// Blocks until `pid` has exited, without reaping it. Returns false if it cannot be waited on
/// (for example it was already reaped elsewhere).
pub(crate) fn wait_for_exit_without_reaping(pid: u32) -> bool {
    let id: libc::id_t = pid;
    loop {
        // SAFETY: siginfo_t is plain data that waitid fills in.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waitid writes into `info`, which outlives the call.
        let result =
            unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if result == 0 {
            return true;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_for_exit_without_reaping_leaves_the_child_reapable() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn sh");
        assert!(wait_for_exit_without_reaping(child.id()));
        // Not reaped yet: the pid still exists as a zombie.
        let pid = libc::pid_t::try_from(child.id()).expect("pid fits");
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        let status = child.wait().expect("reap");
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn spawn_runs_program_on_a_controlling_terminal() {
        let config =
            TerminalConfig::new("/bin/sh").args(["-c", "test -t 0 && test -t 1 && exit 5"]);
        let mut spawned = spawn(&config).expect("spawn");
        let status = spawned.child.wait().expect("wait");
        assert_eq!(status.code(), Some(5));
    }
}
