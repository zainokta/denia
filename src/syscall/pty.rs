//! PTY master/slave allocation and an async wrapper over the master fd. Used by
//! the service console (ADR-033) to give an interactive shell a real terminal.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use tokio::io::unix::AsyncFd;

/// Async, non-blocking handle over a PTY master fd.
#[derive(Debug)]
pub struct PtyMaster {
    inner: AsyncFd<OwnedFd>,
}

impl PtyMaster {
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        set_nonblocking(fd.as_raw_fd())?;
        Ok(Self {
            inner: AsyncFd::new(fd)?,
        })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.inner.get_ref().as_raw_fd()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(self.raw_fd(), libc::TIOCSWINSZ, &winsize) };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl tokio::io::AsyncRead for PtyMaster {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        loop {
            let mut guard = match self.inner.poll_read_ready(cx) {
                std::task::Poll::Ready(result) => result?,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            let dst = buf.initialize_unfilled();
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::read(inner.get_ref().as_raw_fd(), dst.as_mut_ptr().cast(), dst.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return std::task::Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return std::task::Poll::Ready(Err(error)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl tokio::io::AsyncWrite for PtyMaster {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        loop {
            let mut guard = match self.inner.poll_write_ready(cx) {
                std::task::Poll::Ready(result) => result?,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::write(inner.get_ref().as_raw_fd(), bytes.as_ptr().cast(), bytes.len())
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return std::task::Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// Allocate a PTY pair, returning the async master and the raw slave fd to hand
/// to the console child process.
pub fn open_pty(cols: u16, rows: u16) -> io::Result<(PtyMaster, OwnedFd)> {
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if master < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    if unsafe { libc::grantpt(master.as_raw_fd()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::unlockpt(master.as_raw_fd()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let slave_name = pts_name(master.as_raw_fd())?;
    let slave = unsafe {
        libc::open(
            slave_name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(io::Error::last_os_error());
    }
    let pty = PtyMaster::new(master)?;
    pty.resize(cols, rows)?;
    Ok((pty, unsafe { OwnedFd::from_raw_fd(slave) }))
}

fn pts_name(fd: RawFd) -> io::Result<std::ffi::CString> {
    let mut buf = vec![0_i8; 128];
    let rc = unsafe { libc::ptsname_r(fd, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let bytes = buf[..len].iter().map(|b| *b as u8).collect::<Vec<_>>();
    std::ffi::CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pty name contained nul"))
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[tokio::test]
    async fn open_pty_creates_master_and_slave() {
        let (master, slave) = open_pty(80, 24).unwrap();
        assert!(master.raw_fd() >= 0);
        assert!(slave.as_raw_fd() >= 0);
    }

    #[tokio::test]
    async fn resize_accepts_valid_dimensions() {
        let (master, _slave) = open_pty(80, 24).unwrap();
        master.resize(120, 32).unwrap();
    }
}
