//! Child processes with a deadline (the `osascript` fallbacks).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessError {
    Spawn(String),
    TimedOut,
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::Spawn(message) => write!(f, "could not start: {message}"),
            ProcessError::TimedOut => f.write_str("timed out"),
        }
    }
}

/// Runs `program` with `args`, stdin closed, and kills it at `timeout`.
pub fn run_with_timeout(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<ProcessOutput, ProcessError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| ProcessError::Spawn(error.to_string()))?;

    // Drained on their own threads so a chatty child can't block on a full pipe.
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => break None,
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ProcessError::TimedOut);
    };
    let collect = |handle: Option<std::thread::JoinHandle<String>>| {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    Ok(ProcessOutput {
        code: status.code(),
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_output_and_exit_code() {
        let output = run_with_timeout(
            "/bin/sh",
            &["-c", "echo hi; echo err >&2; exit 3"],
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.code, Some(3));
        assert_eq!(output.stdout, "hi\n");
        assert_eq!(output.stderr, "err\n");
    }

    #[test]
    fn kills_a_child_past_its_deadline() {
        let started = Instant::now();
        let result = run_with_timeout("/bin/sleep", &["5"], Duration::from_millis(100));
        assert_eq!(result, Err(ProcessError::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn reports_a_missing_program() {
        assert!(matches!(
            run_with_timeout("/nonexistent/program", &[], Duration::from_secs(1)),
            Err(ProcessError::Spawn(_))
        ));
    }
}
