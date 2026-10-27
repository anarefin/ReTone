//! Spawning llama-server: a [`ProcessSpawner`] seam so lifecycle logic runs headless in tests,
//! the real spawner (own process group, piped output, exit watched by a task),
//! signals to the whole group, and the free-port probe.

use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What to run.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Added to (or replacing) the inherited environment.
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
}

impl SpawnSpec {
    /// The value following `flag` in the argument list.
    pub fn arg_value(&self, flag: &str) -> Option<&str> {
        let i = self.args.iter().position(|a| a == flag)?;
        self.args.get(i + 1).map(String::as_str)
    }

    pub fn has_arg(&self, flag: &str) -> bool {
        self.args.iter().any(|a| a == flag)
    }
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

type Signaller = Arc<dyn Fn(i32) + Send + Sync>;

/// A spawned process: its pid, how it ended (once it has), the tail of its
/// stderr, and a way to signal it.
pub struct ProcessHandle {
    pub pid: u32,
    exit: watch::Receiver<Option<ExitInfo>>,
    stderr: Arc<Mutex<String>>,
    signaller: Signaller,
}

/// Bytes of stderr kept for the startup diagnostic.
const STDERR_KEEP: usize = 64 * 1024;

impl ProcessHandle {
    pub fn new(
        pid: u32,
        exit: watch::Receiver<Option<ExitInfo>>,
        stderr: Arc<Mutex<String>>,
        signaller: Signaller,
    ) -> Self {
        Self {
            pid,
            exit,
            stderr,
            signaller,
        }
    }

    pub fn exited(&self) -> Option<ExitInfo> {
        *self.exit.borrow()
    }

    /// Resolves when the process has ended.
    pub async fn wait(&self) -> Option<ExitInfo> {
        let mut rx = self.exit.clone();
        rx.wait_for(Option::is_some).await.ok().and_then(|v| *v)
    }

    /// Signals the process (and its group) unless it already ended, so a
    /// reused pid is never hit.
    pub fn signal(&self, signal: i32) {
        if self.exited().is_none() {
            (self.signaller)(signal);
        }
    }

    pub fn stderr(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

pub trait ProcessSpawner: Send + Sync {
    fn spawn(&self, spec: SpawnSpec) -> BoxFuture<'static, io::Result<ProcessHandle>>;
}

/// Signals the process group `pid` leads, falling back to the pid alone.
pub fn signal_group(pid: u32, signal: i32) {
    let pid = pid as libc::pid_t;
    // SAFETY: plain kill(2) calls; a dead pid or group only yields ESRCH.
    unsafe {
        if libc::kill(-pid, signal) != 0 {
            libc::kill(pid, signal);
        }
    }
}

/// Spawns real processes: own process group (detached from the app's, so a
/// group signal reaches any grandchildren), stdin closed, output piped to the
/// log, and a task that records the exit.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioSpawner;

impl ProcessSpawner for TokioSpawner {
    fn spawn(&self, spec: SpawnSpec) -> BoxFuture<'static, io::Result<ProcessHandle>> {
        Box::pin(async move {
            let mut command = tokio::process::Command::new(&spec.program);
            command
                .args(&spec.args)
                .envs(spec.env.iter().map(|(k, v)| (k, v)))
                .current_dir(&spec.cwd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .process_group(0)
                .kill_on_drop(false);
            let mut child = command.spawn()?;
            let pid = child
                .id()
                .ok_or_else(|| io::Error::other("process has no pid"))?;

            let stderr_buffer = Arc::new(Mutex::new(String::new()));
            if let Some(stdout) = child.stdout.take() {
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stdout).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(target: "llama", "llama-server stdout: {line}");
                    }
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let buffer = stderr_buffer.clone();
                tokio::spawn(async move {
                    let mut lines = BufReader::new(stderr).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        tracing::debug!(target: "llama", "llama-server stderr: {line}");
                        if let Ok(mut kept) = buffer.lock() {
                            kept.push_str(&line);
                            kept.push('\n');
                            if kept.len() > STDERR_KEEP {
                                let mut cut = kept.len() - STDERR_KEEP;
                                while !kept.is_char_boundary(cut) {
                                    cut += 1;
                                }
                                kept.drain(..cut);
                            }
                        }
                    }
                });
            }

            let (exit_tx, exit_rx) = watch::channel(None);
            tokio::spawn(async move {
                use std::os::unix::process::ExitStatusExt;
                let info = match child.wait().await {
                    Ok(status) => ExitInfo {
                        code: status.code(),
                        signal: status.signal(),
                    },
                    Err(_) => ExitInfo {
                        code: None,
                        signal: None,
                    },
                };
                exit_tx.send_replace(Some(info));
            });

            Ok(ProcessHandle::new(
                pid,
                exit_rx,
                stderr_buffer,
                Arc::new(move |signal| signal_group(pid, signal)),
            ))
        })
    }
}

fn try_bind(addr: SocketAddr) -> bool {
    match TcpListener::bind(addr) {
        Ok(_) => true,
        // A family the host lacks (IPv6 disabled) cannot conflict on the port.
        Err(error) => {
            error.kind() == io::ErrorKind::AddrNotAvailable
                || error.raw_os_error() == Some(libc::EAFNOSUPPORT)
        }
    }
}

/// Free on the wildcard addresses and on loopback.
pub fn is_port_available(port: u16) -> bool {
    try_bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        && try_bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))
        && try_bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// The first free port in `start..=end`.
pub fn find_available_port(start: u16, end: u16) -> io::Result<u16> {
    (start..=end)
        .find(|port| is_port_available(*port))
        .ok_or_else(|| io::Error::other(format!("No available ports in range {start}-{end}")))
}

/// Name of a signal number, for the startup diagnostic.
pub fn signal_name(signal: i32) -> String {
    match signal {
        libc::SIGHUP => "SIGHUP".into(),
        libc::SIGINT => "SIGINT".into(),
        libc::SIGQUIT => "SIGQUIT".into(),
        libc::SIGILL => "SIGILL".into(),
        libc::SIGTRAP => "SIGTRAP".into(),
        libc::SIGABRT => "SIGABRT".into(),
        libc::SIGBUS => "SIGBUS".into(),
        libc::SIGFPE => "SIGFPE".into(),
        libc::SIGKILL => "SIGKILL".into(),
        libc::SIGSEGV => "SIGSEGV".into(),
        libc::SIGPIPE => "SIGPIPE".into(),
        libc::SIGTERM => "SIGTERM".into(),
        other => format!("signal {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn spawns_in_its_own_group_and_reports_the_exit() {
        let handle = TokioSpawner
            .spawn(SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "echo oops >&2; exit 3".into()],
                env: vec![],
                cwd: std::env::temp_dir(),
            })
            .await
            .unwrap();
        let exit = tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit.code, Some(3));
        // The stderr reader may trail the exit by a moment.
        for _ in 0..50 {
            if handle.stderr().contains("oops") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(handle.stderr().contains("oops"));
    }

    #[tokio::test]
    async fn a_group_signal_ends_the_process_and_its_children() {
        let handle = TokioSpawner
            .spawn(SpawnSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30 & wait".into()],
                env: vec![("RETONE_TEST".into(), "1".into())],
                cwd: std::env::temp_dir(),
            })
            .await
            .unwrap();
        // SAFETY: getpgid on our own child.
        let group = unsafe { libc::getpgid(handle.pid as libc::pid_t) };
        assert_eq!(group, handle.pid as libc::pid_t, "leads its own group");
        handle.signal(libc::SIGTERM);
        let exit = tokio::time::timeout(Duration::from_secs(5), handle.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exit.signal, Some(libc::SIGTERM));
        let mut alive = true;
        for _ in 0..100 {
            // SAFETY: probing the group with signal 0.
            alive = unsafe { libc::kill(-(handle.pid as libc::pid_t), 0) } == 0;
            if !alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!alive, "the background sleep went with the group");
    }

    #[test]
    fn finds_a_free_port_and_skips_a_taken_one() {
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        assert!(!is_port_available(port));
        drop(taken);
        let free = find_available_port(port, port.saturating_add(20)).unwrap();
        assert!(free >= port);
    }

    #[test]
    fn spec_reads_flag_values() {
        let spec = SpawnSpec {
            program: "x".into(),
            args: vec!["--ctx-size".into(), "16384".into(), "--jinja".into()],
            env: vec![],
            cwd: "/".into(),
        };
        assert_eq!(spec.arg_value("--ctx-size"), Some("16384"));
        assert!(spec.has_arg("--jinja"));
        assert_eq!(spec.arg_value("--port"), None);
        assert_eq!(signal_name(libc::SIGKILL), "SIGKILL");
    }
}
