//! Sidecar bookkeeping: a pid file per running sidecar under
//! `userData/sidecar-pids`, a startup reaper for processes a crashed run left
//! behind, and an ordered shutdown with a deadline on quit.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PID_SUBDIR: &str = "sidecar-pids";

/// llama-server is the only sidecar ReTone spawns; a pid entry is reaped only
/// when the live process's command line still contains one of these.
pub const EXPECTED_BINARY_FRAGMENTS: &[(&str, &[&str])] = &[("llama", &["llama-server"])];

pub const SIGTERM_GRACE: Duration = Duration::from_secs(5);
pub const SIGKILL_GRACE: Duration = Duration::from_secs(1);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(200);
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(8);

/// `userData/sidecar-pids/<name>.pid`. Best effort: the registry is the
/// primary shutdown path; this only recovers from a crash.
#[derive(Debug, Clone)]
pub struct PidFile {
    dir: PathBuf,
}

impl PidFile {
    pub fn new(user_data: &Path) -> Self {
        Self {
            dir: user_data.join(PID_SUBDIR),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.pid"))
    }

    pub fn write(&self, name: &str, pid: u32) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.path(name), pid.to_string());
    }

    pub fn clear(&self, name: &str) {
        let _ = std::fs::remove_file(self.path(name));
    }

    /// Every readable, well-formed entry, sorted by name. Unreadable or
    /// malformed entries are skipped, never fatal.
    pub fn read_all(&self) -> Vec<(String, i32)> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<(String, i32)> = entries
            .flatten()
            .filter_map(|entry| {
                let file = entry.file_name().to_string_lossy().into_owned();
                let name = file.strip_suffix(".pid")?.to_string();
                let raw = std::fs::read_to_string(entry.path()).ok()?;
                let pid: i32 = raw.trim().parse().ok()?;
                (pid > 0).then_some((name, pid))
            })
            .collect();
        out.sort();
        out
    }
}

/// `kill(pid, 0)`: alive, or alive but not ours (EPERM).
pub fn is_process_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence and permission.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn signal(pid: i32, sig: libc::c_int) {
    // SAFETY: sends a signal; a dead pid only yields ESRCH.
    unsafe {
        libc::kill(pid, sig);
    }
}

/// The process's full command line, or "" when it cannot be read.
pub async fn process_command(pid: i32) -> String {
    match tokio::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// Polls until `pid` is gone; `false` when it outlives `timeout`.
pub async fn wait_for_exit(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while is_process_alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(EXIT_POLL_INTERVAL).await;
    }
    true
}

#[derive(Debug, Clone, Copy)]
pub struct ReapGraces {
    pub sigterm: Duration,
    pub sigkill: Duration,
}

impl Default for ReapGraces {
    fn default() -> Self {
        Self {
            sigterm: SIGTERM_GRACE,
            sigkill: SIGKILL_GRACE,
        }
    }
}

async fn kill_stale(name: &str, pid: i32, graces: ReapGraces) -> bool {
    tracing::warn!(target: "sidecar", name, pid, "Reaping stale sidecar");
    signal(pid, libc::SIGTERM);
    if wait_for_exit(pid, graces.sigterm).await {
        return true;
    }
    tracing::warn!(target: "sidecar", name, pid, "Stale sidecar ignored SIGTERM, escalating to SIGKILL");
    signal(pid, libc::SIGKILL);
    if wait_for_exit(pid, graces.sigkill).await {
        return true;
    }
    tracing::error!(target: "sidecar", name, pid, "Stale sidecar survived SIGKILL, keeping PID entry for next launch");
    false
}

/// Kills every sidecar a previous run left behind (SIGTERM, then SIGKILL) and
/// clears its entry. A pid reused by an unrelated process is left alone.
pub async fn reap_stale_sidecars(pid_file: &PidFile, graces: ReapGraces) {
    let reaps = pid_file
        .read_all()
        .into_iter()
        .map(|(name, pid)| async move {
            let fragments = EXPECTED_BINARY_FRAGMENTS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, f)| *f);
            if let Some(fragments) = fragments
                && is_process_alive(pid)
            {
                let command = process_command(pid).await;
                if fragments.iter().any(|f| command.contains(f))
                    && !kill_stale(&name, pid, graces).await
                {
                    return;
                }
            }
            pid_file.clear(&name);
        });
    futures_join_all(reaps).await;
}

type StopFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type StopFn = Box<dyn Fn() -> StopFuture + Send + Sync>;

/// Sidecars to stop on quit.
#[derive(Default)]
pub struct SidecarRegistry {
    sidecars: Mutex<Vec<(String, StopFn)>>,
}

impl SidecarRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F, Fut>(&self, name: &str, stop: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let stop: StopFn = Box::new(move || Box::pin(stop()));
        self.sidecars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((name.to_string(), stop));
    }

    /// Stops every sidecar concurrently, giving up after `deadline`. Returns
    /// whether all of them finished in time.
    pub async fn shutdown_all(&self, deadline: Duration) -> bool {
        let stops: Vec<(String, StopFuture)> = self
            .sidecars
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, stop)| (name.clone(), stop()))
            .collect();
        if stops.is_empty() {
            return true;
        }
        let all = futures_join_all(stops.into_iter().map(|(name, stop)| async move {
            stop.await;
            tracing::debug!(target: "sidecar", name, "Sidecar stopped");
        }));
        tokio::time::timeout(deadline, all).await.is_ok()
    }
}

/// Minimal `join_all` over spawned-free futures (no `futures` dependency).
async fn futures_join_all<F: Future<Output = ()>>(futures: impl IntoIterator<Item = F>) {
    let mut set = Vec::new();
    for future in futures {
        set.push(Box::pin(future));
    }
    std::future::poll_fn(|cx| {
        let mut pending = false;
        set.retain_mut(|future| {
            if future.as_mut().poll(cx).is_pending() {
                pending = true;
                true
            } else {
                false
            }
        });
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncBufReadExt;

    #[test]
    fn an_unreadable_entry_does_not_hide_valid_ones() {
        let dir = tempfile::tempdir().unwrap();
        let pids = PidFile::new(dir.path());
        let pid_dir = dir.path().join(PID_SUBDIR);
        std::fs::create_dir(&pid_dir).unwrap();
        std::fs::write(pid_dir.join("qdrant.pid"), "1234").unwrap();
        std::fs::create_dir(pid_dir.join("broken.pid")).unwrap();
        assert_eq!(pids.read_all(), vec![("qdrant".to_string(), 1234)]);
    }

    #[test]
    fn an_unreadable_registry_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PID_SUBDIR), "not a directory").unwrap();
        assert!(PidFile::new(dir.path()).read_all().is_empty());
    }

    #[test]
    fn missing_malformed_and_unrelated_entries_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let pids = PidFile::new(dir.path());
        assert!(pids.read_all().is_empty());
        let pid_dir = dir.path().join(PID_SUBDIR);
        std::fs::create_dir(&pid_dir).unwrap();
        for (file, body) in [
            ("zero.pid", "0"),
            ("fraction.pid", "12.5"),
            ("text.pid", "not-a-pid"),
            ("notes.txt", "5678"),
        ] {
            std::fs::write(pid_dir.join(file), body).unwrap();
        }
        assert!(pids.read_all().is_empty());
    }

    #[test]
    fn entries_round_trip_through_write_read_and_clear() {
        let dir = tempfile::tempdir().unwrap();
        let pids = PidFile::new(dir.path());
        pids.write("llama", 4321);
        assert_eq!(pids.read_all(), vec![("llama".to_string(), 4321)]);
        pids.clear("llama");
        assert!(pids.read_all().is_empty());
    }

    const GRACES: ReapGraces = ReapGraces {
        sigterm: Duration::from_millis(1000),
        sigkill: Duration::from_millis(1000),
    };

    /// A shell loop whose script name decides whether the reaper recognises it.
    async fn spawn_fake(dir: &Path, script: &str, ignore_sigterm: bool) -> i32 {
        let path = dir.join(script);
        let trap = if ignore_sigterm { "trap '' TERM\n" } else { "" };
        std::fs::write(
            &path,
            format!("{trap}echo ready\nwhile :; do sleep 0.2; done\n"),
        )
        .unwrap();
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg(&path)
            .stdout(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id().unwrap() as i32;
        let mut lines = tokio::io::BufReader::new(child.stdout.take().unwrap()).lines();
        lines.next_line().await.unwrap();
        // Reap the child once it dies so `kill(pid, 0)` sees it gone.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        pid
    }

    struct KillOnDrop(i32);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            signal(self.0, libc::SIGKILL);
        }
    }

    #[tokio::test]
    async fn kills_a_responsive_stale_sidecar_and_clears_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        let pid = spawn_fake(dir.path(), "llama-server-fake.sh", false).await;
        let _guard = KillOnDrop(pid);
        let pids = PidFile::new(dir.path());
        pids.write("llama", pid as u32);

        reap_stale_sidecars(&pids, GRACES).await;

        assert!(wait_for_exit(pid, Duration::from_secs(2)).await);
        assert!(pids.read_all().is_empty());
    }

    #[tokio::test]
    async fn escalates_to_sigkill_when_sigterm_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let pid = spawn_fake(dir.path(), "llama-server-stuck.sh", true).await;
        let _guard = KillOnDrop(pid);
        let pids = PidFile::new(dir.path());
        pids.write("llama", pid as u32);

        reap_stale_sidecars(&pids, GRACES).await;

        assert!(wait_for_exit(pid, Duration::from_secs(2)).await);
        assert!(pids.read_all().is_empty());
    }

    #[tokio::test]
    async fn does_not_kill_a_reused_pid_that_is_no_longer_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let pid = spawn_fake(dir.path(), "other-app.sh", false).await;
        let _guard = KillOnDrop(pid);
        let pids = PidFile::new(dir.path());
        pids.write("llama", pid as u32);

        reap_stale_sidecars(&pids, GRACES).await;

        assert!(is_process_alive(pid));
        assert!(pids.read_all().is_empty());
    }

    #[tokio::test]
    async fn wait_for_exit_tells_a_live_process_from_a_dead_one() {
        let dir = tempfile::tempdir().unwrap();
        let pid = spawn_fake(dir.path(), "llama-server-wait.sh", false).await;
        let _guard = KillOnDrop(pid);
        assert!(!wait_for_exit(pid, Duration::from_millis(400)).await);
        signal(pid, libc::SIGKILL);
        assert!(wait_for_exit(pid, Duration::from_secs(2)).await);
    }

    #[tokio::test]
    async fn clears_entries_for_processes_that_already_exited() {
        let dir = tempfile::tempdir().unwrap();
        let pid = spawn_fake(dir.path(), "llama-server-gone.sh", false).await;
        signal(pid, libc::SIGKILL);
        assert!(wait_for_exit(pid, Duration::from_secs(2)).await);
        let pids = PidFile::new(dir.path());
        pids.write("llama", pid as u32);

        reap_stale_sidecars(&pids, GRACES).await;
        assert!(pids.read_all().is_empty());
    }

    #[tokio::test]
    async fn unknown_sidecar_names_are_cleared_without_signalling() {
        let dir = tempfile::tempdir().unwrap();
        let pid = spawn_fake(dir.path(), "llama-server-unknown.sh", false).await;
        let _guard = KillOnDrop(pid);
        let pids = PidFile::new(dir.path());
        pids.write("qdrant", pid as u32);

        reap_stale_sidecars(&pids, GRACES).await;
        assert!(is_process_alive(pid));
        assert!(pids.read_all().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_all_stops_every_sidecar_within_the_deadline() {
        let registry = SidecarRegistry::new();
        let stopped = Arc::new(AtomicUsize::new(0));
        for name in ["a", "b"] {
            let stopped = stopped.clone();
            registry.register(name, move || {
                let stopped = stopped.clone();
                async move {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    stopped.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
        assert!(registry.shutdown_all(SHUTDOWN_DEADLINE).await);
        assert_eq!(stopped.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_all_gives_up_at_the_deadline() {
        let registry = SidecarRegistry::new();
        registry.register("wedged", || tokio::time::sleep(Duration::from_secs(60)));
        let started = tokio::time::Instant::now();
        assert!(!registry.shutdown_all(SHUTDOWN_DEADLINE).await);
        assert_eq!(started.elapsed(), SHUTDOWN_DEADLINE);
        assert!(SidecarRegistry::new().shutdown_all(SHUTDOWN_DEADLINE).await);
    }
}
