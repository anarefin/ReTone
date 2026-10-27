//! Running a closure on the main thread. Text Input Source calls
//! (`TISGetInputSourceProperty`) assert they run on the main queue on recent
//! macOS, so the keyboard-layout lookup hops there.

use std::sync::mpsc;
use std::time::Duration;

pub fn is_main_thread() -> bool {
    // SAFETY: a plain query of the calling thread.
    unsafe { libc::pthread_main_np() != 0 }
}

/// Runs `work` on the main thread and waits up to `timeout` for its result.
/// On the main thread it runs inline. `None` if the main queue didn't get to it
/// in time (it is not serviced, or is busy).
pub fn run_on_main<R, F>(timeout: Duration, work: F) -> Option<R>
where
    R: Send + 'static,
    F: FnOnce() -> R + Send + 'static,
{
    if is_main_thread() {
        return Some(work());
    }
    let (sender, receiver) = mpsc::channel();
    dispatch2::DispatchQueue::main().exec_async(move || {
        let _ = sender.send(work());
    });
    receiver.recv_timeout(timeout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_threads_are_not_the_main_thread_and_time_out() {
        // The test harness doesn't service the main queue.
        assert!(!is_main_thread());
        assert_eq!(run_on_main(Duration::from_millis(50), || 1), None);
    }
}
