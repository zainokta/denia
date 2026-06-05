use std::{
    ffi::OsString,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{ExitStatus, Stdio},
};

use thiserror::Error;
use tokio::{
    io,
    net::{TcpStream, UnixListener},
    process::Command,
};

use crate::syscall::{self, caps};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketProxyConfig {
    pub listen_socket: PathBuf,
    pub connect_host: String,
    pub connect_port: u16,
    pub workdir: Option<PathBuf>,
    pub child_argv: Vec<OsString>,
}

#[derive(Debug, Error)]
pub enum SocketProxyError {
    #[error("missing socket proxy argument: {name}")]
    MissingArgument { name: &'static str },
    #[error("invalid socket proxy argument: {value}")]
    InvalidArgument { value: String },
    #[error("socket proxy child argv is empty")]
    EmptyChildArgv,
    #[error("socket proxy child exited with status {status}")]
    ChildFailed { status: ExitStatus },
    #[error("socket proxy hardening failed: {0}")]
    Hardening(#[from] syscall::SyscallError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn parse_args<I>(args: I) -> Result<SocketProxyConfig, SocketProxyError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut listen_socket = None;
    let mut connect = None;
    let mut workdir = None;
    let mut child_argv = Vec::new();

    while let Some(arg) = args.next() {
        if arg == "--" {
            child_argv.extend(args);
            break;
        }
        if arg == "--listen" {
            listen_socket = Some(
                args.next()
                    .ok_or(SocketProxyError::MissingArgument { name: "--listen" })?
                    .into(),
            );
            continue;
        }
        if arg == "--connect" {
            connect = Some(
                args.next()
                    .ok_or(SocketProxyError::MissingArgument { name: "--connect" })?,
            );
            continue;
        }
        if arg == "--workdir" {
            workdir = Some(
                args.next()
                    .ok_or(SocketProxyError::MissingArgument { name: "--workdir" })?
                    .into(),
            );
            continue;
        }
        return Err(SocketProxyError::InvalidArgument {
            value: arg.to_string_lossy().to_string(),
        });
    }

    let listen_socket =
        listen_socket.ok_or(SocketProxyError::MissingArgument { name: "--listen" })?;
    let connect = connect.ok_or(SocketProxyError::MissingArgument { name: "--connect" })?;
    let connect = connect.to_string_lossy();
    let (connect_host, connect_port) =
        connect
            .rsplit_once(':')
            .ok_or_else(|| SocketProxyError::InvalidArgument {
                value: connect.to_string(),
            })?;
    let connect_port =
        connect_port
            .parse::<u16>()
            .map_err(|_| SocketProxyError::InvalidArgument {
                value: connect.to_string(),
            })?;
    if child_argv.is_empty() {
        return Err(SocketProxyError::EmptyChildArgv);
    }

    Ok(SocketProxyConfig {
        listen_socket,
        connect_host: connect_host.to_string(),
        connect_port,
        workdir,
        child_argv,
    })
}

pub async fn run_from_args<I>(args: I) -> Result<(), SocketProxyError>
where
    I: IntoIterator<Item = OsString>,
{
    run(parse_args(args)?).await
}

pub async fn run(config: SocketProxyConfig) -> Result<(), SocketProxyError> {
    // The launcher (child_exec) brings `lo` up privileged before execve, since
    // this process is capless post-execve. Best-effort here so a leftover
    // SIOCSIFFLAGS EPERM on the already-up `lo` is not fatal; still works when
    // socket-proxy is run standalone with capabilities.
    let _ = bring_loopback_up();
    if let Some(parent) = config.listen_socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::remove_file(&config.listen_socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(SocketProxyError::Io(error)),
    }

    let listener = UnixListener::bind(&config.listen_socket)?;
    std::fs::set_permissions(
        &config.listen_socket,
        std::fs::Permissions::from_mode(0o666),
    )?;
    caps::set_no_new_privs()?;
    caps::drop_bounding_caps()?;
    // Install the seccomp denylist before spawning so the workload child inherits
    // it. Long-running services defer hardening to this proxy, so without this
    // call the workload would run with no syscall filter at all (F-4).
    syscall::seccomp::install_filter()?;
    let mut command = Command::new(&config.child_argv[0]);
    command.args(&config.child_argv[1..]).stdin(Stdio::null());
    if let Some(workdir) = &config.workdir {
        command.current_dir(workdir);
    }
    let mut child = command.spawn()?;

    loop {
        tokio::select! {
            status = child.wait() => {
                let status = status?;
                if status.success() {
                    return Ok(());
                }
                return Err(SocketProxyError::ChildFailed { status });
            }
            accepted = listener.accept() => {
                let (mut unix, _) = accepted?;
                let connect_host = config.connect_host.clone();
                let connect_port = config.connect_port;
                tokio::spawn(async move {
                    match TcpStream::connect((connect_host.as_str(), connect_port)).await {
                        Ok(mut tcp) => {
                            let _ = io::copy_bidirectional(&mut unix, &mut tcp).await;
                        }
                        Err(e) => eprintln!(
                            "socket-proxy: upstream connect {connect_host}:{connect_port} failed: {e} \
                             (is the workload listening on that port?)"
                        ),
                    }
                });
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn bring_loopback_up() -> std::io::Result<()> {
    let mut request = IfReq::new("lo");
    let socket = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if socket < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let result = unsafe {
        if libc::ioctl(socket, libc::SIOCGIFFLAGS, &mut request) < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            request.ifr_flags |= libc::IFF_UP as libc::c_short;
            if libc::ioctl(socket, libc::SIOCSIFFLAGS, &request) < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    };
    let close_result = unsafe { libc::close(socket) };
    if result.is_ok() && close_result < 0 {
        return Err(std::io::Error::last_os_error());
    }
    result
}

#[cfg(not(target_os = "linux"))]
fn bring_loopback_up() -> std::io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct IfReq {
    ifr_name: [libc::c_char; libc::IFNAMSIZ],
    ifr_flags: libc::c_short,
    _padding: [u8; 22],
}

#[cfg(target_os = "linux")]
impl IfReq {
    fn new(name: &str) -> Self {
        let mut request = Self {
            ifr_name: [0; libc::IFNAMSIZ],
            ifr_flags: 0,
            _padding: [0; 22],
        };
        for (target, source) in request
            .ifr_name
            .iter_mut()
            .zip(name.as_bytes().iter().copied())
        {
            *target = source as libc::c_char;
        }
        request
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_reads_proxy_and_child_contract() {
        let config = parse_args([
            OsString::from("--listen"),
            OsString::from("/run/denia/service.sock"),
            OsString::from("--connect"),
            OsString::from("127.0.0.1:3000"),
            OsString::from("--workdir"),
            OsString::from("/app"),
            OsString::from("--"),
            OsString::from("/bin/web"),
        ])
        .expect("config");

        assert_eq!(
            config.listen_socket,
            PathBuf::from("/run/denia/service.sock")
        );
        assert_eq!(config.connect_host, "127.0.0.1");
        assert_eq!(config.connect_port, 3000);
        assert_eq!(config.workdir, Some(PathBuf::from("/app")));
        assert_eq!(config.child_argv, vec![OsString::from("/bin/web")]);
    }

    #[test]
    fn parse_args_requires_child_argv() {
        let error = parse_args([
            OsString::from("--listen"),
            OsString::from("/run/denia/service.sock"),
            OsString::from("--connect"),
            OsString::from("127.0.0.1:3000"),
            OsString::from("--"),
        ])
        .expect_err("child argv");

        assert!(matches!(error, SocketProxyError::EmptyChildArgv));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn loopback_ioctl_request_uses_lo_interface_name() {
        let request = IfReq::new("lo");
        assert_eq!(request.ifr_name[0], b'l' as libc::c_char);
        assert_eq!(request.ifr_name[1], b'o' as libc::c_char);
        assert_eq!(request.ifr_name[2], 0);
    }
}
