//! Autorepeat gate. A held key re-fires the
//! global shortcut on every autorepeat (macOS: first after 225–375 ms, then
//! every 30–90 ms) and never reports the release. Fires inside the window of
//! the previous fire are repeats and renew the window, so one hold is one press
//! however long it lasts.

use std::time::Duration;

pub const HOTKEY_REPEAT_WINDOW: Duration = Duration::from_millis(600);

#[derive(Debug, Clone)]
pub struct RepeatGate {
    window: Duration,
    last_fire: Option<Duration>,
}

impl Default for RepeatGate {
    fn default() -> Self {
        Self::new(HOTKEY_REPEAT_WINDOW)
    }
}

impl RepeatGate {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            last_fire: None,
        }
    }

    /// Records a fire at `now` (any monotonic clock) and says whether it is a
    /// new press rather than an autorepeat.
    pub fn is_press(&mut self, now: Duration) -> bool {
        let is_repeat = self
            .last_fire
            .is_some_and(|last| now.saturating_sub(last) < self.window);
        self.last_fire = Some(now);
        !is_repeat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presses(times: &[u64]) -> Vec<bool> {
        let mut gate = RepeatGate::default();
        times
            .iter()
            .map(|&t| gate.is_press(Duration::from_millis(t)))
            .collect()
    }

    #[test]
    fn a_held_key_is_one_press_autorepeats_renew_the_window() {
        // macOS default: first repeat 375 ms after the press, then every 90 ms.
        let times = [0, 375, 465, 555, 645, 735, 825, 915, 1005];
        let mut expected = vec![false; times.len()];
        expected[0] = true;
        assert_eq!(presses(&times), expected);
    }

    #[test]
    fn a_deliberate_second_press_after_the_window_counts() {
        assert_eq!(presses(&[0, 700, 1300]), [true, true, true]);
    }

    #[test]
    fn a_quick_second_tap_inside_the_window_is_a_repeat() {
        assert_eq!(presses(&[0, 300]), [true, false]);
    }
}
