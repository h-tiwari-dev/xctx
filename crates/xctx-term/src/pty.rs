//! Opening a pseudo-terminal and starting a program on it.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
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
}

fn winsize(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols.saturating_mul(cell_width),
        ws_ypixel: rows.saturating_mul(cell_height),
    }
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor we own; no memory is passed.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Opens a PTY pair sized to `cols` x `rows`, returning (master, slave).
fn open_pty(
    cols: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
) -> Result<(OwnedFd, OwnedFd), TermError> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let mut size = winsize(cols, rows, cell_width, cell_height);
    // SAFETY: openpty writes two descriptors into the provided integers; the name and termios
    // pointers may be null.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if result != 0 {
        return Err(TermError::OpenPty(io::Error::last_os_error()));
    }
    // SAFETY: openpty succeeded, so both descriptors are open and owned by us alone.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

    // Neither end may leak into the child beyond its stdio, nor into other programs the app
    // starts.
    set_cloexec(master.as_raw_fd()).map_err(TermError::ConfigurePty)?;
    set_cloexec(slave.as_raw_fd()).map_err(TermError::ConfigurePty)?;

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
    // Close our copies of the slave (the parent's and the Command's) so reading the master
    // reports end-of-file once the program and its children have exited.
    drop(command);
    drop(slave);

    Ok(SpawnedPty {
        master: File::from(master),
        child,
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
