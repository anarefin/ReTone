//! Resumable file downloads for the model manager: a `.tmp` sibling resumed with `Range`, up to 3 retries with
//! exponential backoff for transient failures, a 30 s stall timeout, progress
//! throttled to 100 ms, an atomic rename at the end, plus the disk-space check
//! and the 24 h stale-temp cleanup.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

pub const USER_AGENT: &str = "ReTone/1.0";
pub const DEFAULT_MAX_RETRIES: u32 = 3;
pub const STALE_TMP_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// A cancel signal shared by a download and whoever may cancel it.
#[derive(Debug, Clone)]
pub struct CancelToken(Arc<watch::Sender<bool>>);

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }

    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once [`cancel`](Self::cancel) has been called.
    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|cancelled| *cancelled).await;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DownloadTimings {
    pub stall: Duration,
    pub backoff_base: Duration,
    pub max_backoff: Duration,
    pub progress_throttle: Duration,
}

impl Default for DownloadTimings {
    fn default() -> Self {
        Self {
            stall: Duration::from_secs(30),
            backoff_base: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            progress_throttle: Duration::from_millis(100),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    #[error("Download cancelled")]
    Cancelled,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("Certificate error: {0}")]
    Tls(String),
    #[error("Download stalled — no data received for {0}s")]
    Stalled(u64),
    #[error("Download incomplete: received {received} of {total} bytes")]
    Incomplete { received: u64, total: u64 },
    /// A transient transport failure (reset, refused, DNS, timeout).
    #[error("{0}")]
    Network(String),
    /// A local file-system failure.
    #[error("{0}")]
    Io(String),
}

impl DownloadError {
    /// Same rule as the JS: aborts, HTTP statuses, TLS and local I/O failures
    /// are final; transport hiccups, stalls and short bodies are retried.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Network(_) | Self::Stalled(_) | Self::Incomplete { .. }
        )
    }

    /// The JS error `code`, where the renderer keys a message on it.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Stalled(_) => Some("ETIMEDOUT"),
            Self::Tls(_) => Some("TLS_ERROR"),
            Self::Incomplete { .. } => Some("ERR_DOWNLOAD_INCOMPLETE"),
            _ => None,
        }
    }
}

fn describe(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn classify(error: &reqwest::Error) -> DownloadError {
    let text = describe(error);
    let lower = text.to_ascii_lowercase();
    if lower.contains("certificate") || lower.contains("tls") || lower.contains("issuer") {
        DownloadError::Tls(text)
    } else {
        DownloadError::Network(text)
    }
}

fn io_error(error: std::io::Error) -> DownloadError {
    DownloadError::Io(error.to_string())
}

pub type ProgressFn<'a> = &'a (dyn Fn(u64, u64) + Send + Sync);

pub struct DownloadOptions<'a> {
    pub cancel: &'a CancelToken,
    /// `(downloaded_bytes, total_bytes)`; total is 0 when unknown.
    pub on_progress: Option<ProgressFn<'a>>,
    pub max_retries: u32,
    pub expected_size: u64,
    pub timings: DownloadTimings,
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

fn content_length(response: &reqwest::Response) -> u64 {
    header(response, "content-length")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

async fn attempt(
    client: &reqwest::Client,
    url: &str,
    temp: &Path,
    start_offset: u64,
    options: &DownloadOptions<'_>,
) -> Result<(), DownloadError> {
    if options.cancel.is_cancelled() {
        return Err(DownloadError::Cancelled);
    }
    let mut request = client.get(url).header("User-Agent", USER_AGENT);
    if start_offset > 0 {
        request = request.header("Range", format!("bytes={start_offset}-"));
    }
    let response = tokio::select! {
        response = request.send() => response.map_err(|e| classify(&e))?,
        () = options.cancel.cancelled() => return Err(DownloadError::Cancelled),
    };

    let status = response.status().as_u16();
    let mut open = tokio::fs::OpenOptions::new();
    open.create(true);
    let (mut downloaded, mut total) = match status {
        // The server ignored the Range header: start over.
        200 => {
            open.write(true).truncate(true);
            (0, content_length(&response))
        }
        206 => {
            open.append(true);
            let total = header(&response, "content-range")
                .and_then(|range| range.rsplit('/').next()?.trim().parse::<u64>().ok())
                .unwrap_or_else(|| start_offset + content_length(&response));
            (start_offset, total)
        }
        _ => return Err(DownloadError::Http(status)),
    };
    if total == 0 && options.expected_size > 0 {
        total = options.expected_size;
    }

    let mut file = open.open(temp).await.map_err(io_error)?;
    let mut response = response;
    let mut last_progress: Option<Instant> = None;
    let stall_secs = options.timings.stall.as_secs();
    loop {
        let chunk = tokio::select! {
            chunk = tokio::time::timeout(options.timings.stall, response.chunk()) => match chunk {
                Err(_) => return Err(DownloadError::Stalled(stall_secs)),
                Ok(chunk) => chunk.map_err(|e| classify(&e))?,
            },
            () = options.cancel.cancelled() => return Err(DownloadError::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        file.write_all(&chunk).await.map_err(io_error)?;
        downloaded += chunk.len() as u64;
        if let Some(report) = options.on_progress {
            let due = last_progress
                .is_none_or(|at| at.elapsed() >= options.timings.progress_throttle)
                || (total > 0 && downloaded >= total);
            if due {
                last_progress = Some(Instant::now());
                report(downloaded, total);
            }
        }
    }
    file.flush().await.map_err(io_error)?;
    drop(file);

    if total > 0 && downloaded < total {
        return Err(DownloadError::Incomplete {
            received: downloaded,
            total,
        });
    }
    Ok(())
}

fn temp_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

async fn file_size(path: &Path) -> u64 {
    tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0)
}

async fn finalize(temp: &Path, dest: &Path) -> Result<(), DownloadError> {
    match tokio::fs::rename(temp, dest).await {
        Ok(()) => Ok(()),
        // EXDEV: across volumes, copy then remove.
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            tokio::fs::copy(temp, dest).await.map_err(io_error)?;
            let _ = tokio::fs::remove_file(temp).await;
            Ok(())
        }
        Err(error) => Err(io_error(error)),
    }
}

fn backoff(timings: &DownloadTimings, retry: u32) -> Duration {
    timings
        .backoff_base
        .saturating_mul(2u32.saturating_pow(retry))
        .min(timings.max_backoff)
}

/// Downloads `url` to `dest` through `dest.tmp`, resuming a temp file left by
/// an earlier attempt. The temp file is removed on cancel and on final failure.
pub async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    options: DownloadOptions<'_>,
) -> Result<PathBuf, DownloadError> {
    let temp = temp_path(dest);
    tracing::info!(target: "models", url, dest = %dest.display(), "Download starting");

    let mut start_offset = file_size(&temp).await;
    if start_offset > 0 {
        tracing::info!(target: "models", start_offset, "Resuming download");
    }

    let mut last_error = DownloadError::Network("download did not run".into());
    for retry in 0..=options.max_retries {
        if options.cancel.is_cancelled() {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(DownloadError::Cancelled);
        }
        if retry > 0 {
            let delay = backoff(&options.timings, retry - 1);
            tracing::info!(target: "models", attempt = retry, ?delay, start_offset, "Retrying download");
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                () = options.cancel.cancelled() => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    return Err(DownloadError::Cancelled);
                }
            }
            start_offset = file_size(&temp).await;
        }

        match attempt(client, url, &temp, start_offset, &options).await {
            Ok(()) => {
                finalize(&temp, dest).await?;
                tracing::info!(target: "models", dest = %dest.display(), "Download complete");
                return Ok(dest.to_path_buf());
            }
            Err(DownloadError::Cancelled) => {
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(DownloadError::Cancelled);
            }
            Err(error) if !error.is_retryable() || retry >= options.max_retries => {
                tracing::error!(target: "models", url, %error, code = error.code(), "Download failed");
                let _ = tokio::fs::remove_file(&temp).await;
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(target: "models", attempt = retry + 1, %error, "Download attempt failed");
                last_error = error;
            }
        }
    }
    let _ = tokio::fs::remove_file(&temp).await;
    Err(last_error)
}

/// Removes `.tmp` files and `temp-extract-*` dirs older than `max_age`.
pub fn cleanup_stale_downloads(directory: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".tmp") && !name.starts_with("temp-extract-") {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        if age <= max_age {
            continue;
        }
        let removed = if meta.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if removed.is_ok() {
            tracing::info!(target: "models", path = %path.display(), "Cleaned up stale download artifact");
        }
    }
}

/// Free space for `directory` against `required_bytes`. When it cannot be
/// read, the check passes (as in the JS) with `available = u64::MAX`.
pub fn check_disk_space(directory: &Path, required_bytes: u64) -> (bool, u64) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = CString::new(directory.as_os_str().as_bytes()) else {
        return (true, u64::MAX);
    };
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stats` is a
    // writable statfs owned by this frame.
    let result = unsafe { libc::statfs(path.as_ptr(), &mut stats) };
    if result != 0 {
        return (true, u64::MAX);
    }
    let available = stats.f_bavail.saturating_mul(stats.f_bsize as u64);
    (available >= required_bytes, available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockServer, Reply};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fast() -> DownloadTimings {
        DownloadTimings {
            stall: Duration::from_millis(300),
            backoff_base: Duration::from_millis(5),
            max_backoff: Duration::from_millis(20),
            progress_throttle: Duration::from_millis(100),
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    async fn run(
        url: &str,
        dest: &Path,
        cancel: &CancelToken,
        progress: Option<ProgressFn<'_>>,
    ) -> Result<PathBuf, DownloadError> {
        download_file(
            &client(),
            url,
            dest,
            DownloadOptions {
                cancel,
                on_progress: progress,
                max_retries: DEFAULT_MAX_RETRIES,
                expected_size: 0,
                timings: fast(),
            },
        )
        .await
    }

    #[tokio::test]
    async fn downloads_to_a_temp_file_then_renames_it() {
        let body = vec![7u8; 200_000];
        let served = body.clone();
        let server = MockServer::start(move |req| {
            assert_eq!(req.method, "GET");
            assert_eq!(req.header("user-agent"), Some(USER_AGENT));
            Reply::bytes(200, served.clone())
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        let seen = Mutex::new(Vec::new());
        let report = |done: u64, total: u64| seen.lock().unwrap().push((done, total));

        run(&server.url("/m"), &dest, &CancelToken::new(), Some(&report))
            .await
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert!(!temp_path(&dest).exists());
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.last(),
            Some(&(200_000, 200_000)),
            "the final byte is reported"
        );
    }

    #[tokio::test]
    async fn resumes_a_partial_temp_file_with_a_range_request() {
        let body: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let served = body.clone();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let seen_ranges = ranges.clone();
        let server = MockServer::start(move |req| {
            let range = req.header("range").map(str::to_string);
            seen_ranges.lock().unwrap().push(range.clone());
            match range.and_then(|r| {
                r.strip_prefix("bytes=")?
                    .strip_suffix('-')?
                    .parse::<usize>()
                    .ok()
            }) {
                Some(from) => Reply::bytes(206, served[from..].to_vec()).with_header(
                    "Content-Range",
                    &format!("bytes {from}-{}/{}", served.len() - 1, served.len()),
                ),
                None => Reply::bytes(200, served.clone()),
            }
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        std::fs::write(temp_path(&dest), &body[..40_000]).unwrap();

        run(&server.url("/m"), &dest, &CancelToken::new(), None)
            .await
            .unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(
            ranges.lock().unwrap().as_slice(),
            &[Some("bytes=40000-".to_string())]
        );
    }

    #[tokio::test]
    async fn restarts_from_zero_when_the_server_ignores_the_range() {
        let body = vec![3u8; 50_000];
        let served = body.clone();
        let server = MockServer::start(move |_| Reply::bytes(200, served.clone())).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        std::fs::write(temp_path(&dest), vec![9u8; 10_000]).unwrap();

        run(&server.url("/m"), &dest, &CancelToken::new(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[tokio::test]
    async fn retries_a_cut_off_body_and_resumes_where_it_stopped() {
        let body: Vec<u8> = (0..60_000u32).map(|i| (i % 13) as u8).collect();
        let served = body.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let server = MockServer::start(move |req| {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                // Promise everything, send a third, close.
                return Reply::truncated(200, served.clone(), 20_000);
            }
            let from: usize = req
                .header("range")
                .and_then(|r| r.strip_prefix("bytes=")?.strip_suffix('-')?.parse().ok())
                .unwrap_or(0);
            Reply::bytes(206, served[from..].to_vec()).with_header(
                "Content-Range",
                &format!("bytes {from}-{}/{}", served.len() - 1, served.len()),
            )
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");

        run(&server.url("/m"), &dest, &CancelToken::new(), None)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn an_http_error_is_final_and_leaves_no_temp_file() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let server = MockServer::start(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Reply::bytes(404, b"Entry not found".to_vec())
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");

        let error = run(&server.url("/m"), &dest, &CancelToken::new(), None)
            .await
            .unwrap_err();
        assert_eq!(error, DownloadError::Http(404));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "HTTP errors are not retried"
        );
        assert!(!temp_path(&dest).exists());
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_stalled_body_is_retried_then_reported() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let server = MockServer::start(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            Reply::stall(200, 1000, 100)
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");

        let error = run(&server.url("/m"), &dest, &CancelToken::new(), None)
            .await
            .unwrap_err();
        assert!(matches!(error, DownloadError::Stalled(_)));
        assert_eq!(error.code(), Some("ETIMEDOUT"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            4,
            "one try plus three retries"
        );
        assert!(!temp_path(&dest).exists());
    }

    #[tokio::test]
    async fn cancelling_mid_body_removes_the_temp_file() {
        let server = MockServer::start(|_| Reply::stall(200, 1_000_000, 4096)).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.gguf");
        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let notify = started.clone();
        let report = move |_: u64, _: u64| notify.notify_one();

        let url = server.url("/m");
        let task = run(&url, &dest, &cancel, Some(&report));
        let (result, ()) = tokio::join!(task, async {
            started.notified().await;
            canceller.cancel();
        });
        assert_eq!(result.unwrap_err(), DownloadError::Cancelled);
        assert!(!temp_path(&dest).exists());
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_refused_connection_is_a_retryable_network_error() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let dir = tempfile::tempdir().unwrap();
        let error = run(
            &format!("http://127.0.0.1:{port}/m"),
            &dir.path().join("m"),
            &CancelToken::new(),
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DownloadError::Network(_)), "{error:?}");
        assert!(error.is_retryable());
    }

    #[test]
    fn backoff_doubles_and_is_capped() {
        let timings = DownloadTimings::default();
        assert_eq!(backoff(&timings, 0), Duration::from_secs(1));
        assert_eq!(backoff(&timings, 1), Duration::from_secs(2));
        assert_eq!(backoff(&timings, 2), Duration::from_secs(4));
        assert_eq!(backoff(&timings, 10), Duration::from_secs(30));
    }

    #[test]
    fn cleans_up_only_old_temp_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let old_tmp = dir.path().join("a.gguf.tmp");
        let new_tmp = dir.path().join("b.gguf.tmp");
        let old_extract = dir.path().join("temp-extract-1");
        let model = dir.path().join("c.gguf");
        std::fs::write(&old_tmp, "x").unwrap();
        std::fs::write(&new_tmp, "x").unwrap();
        std::fs::create_dir(&old_extract).unwrap();
        std::fs::write(&model, "x").unwrap();
        let day_ago = SystemTime::now() - Duration::from_secs(25 * 3600);
        for path in [&old_tmp, &old_extract, &model] {
            std::fs::File::options()
                .write(path.is_file())
                .read(true)
                .open(path)
                .unwrap()
                .set_modified(day_ago)
                .unwrap();
        }

        cleanup_stale_downloads(dir.path(), STALE_TMP_AGE);

        assert!(!old_tmp.exists());
        assert!(!old_extract.exists());
        assert!(new_tmp.exists(), "a fresh temp file may still be resumed");
        assert!(model.exists(), "models are never cleaned up");
        cleanup_stale_downloads(&dir.path().join("missing"), STALE_TMP_AGE);
    }

    #[test]
    fn disk_space_reads_the_volume_and_passes_when_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let (ok, available) = check_disk_space(dir.path(), 1);
        assert!(ok);
        assert!(available > 0 && available < u64::MAX);
        assert!(!check_disk_space(dir.path(), u64::MAX - 1).0);
        assert_eq!(
            check_disk_space(&dir.path().join("missing"), u64::MAX - 1),
            (true, u64::MAX)
        );
    }
}
