//! What the tone picker shows, and how a hotkey press arrives at it: the
//! capture-to-payload mapping, the
//! capture flow (dormant-tree re-read, whole-field select-all) and the picker's
//! placement next to the cursor.

use serde::Serialize;
use std::future::Future;
use std::time::Duration;

/// The picker sits this far right of and below the cursor.
pub const CURSOR_OFFSET: f64 = 8.0;
/// Some apps apply a synthetic ⌘A a beat after the keystroke lands.
pub const SELECT_ALL_SETTLE: Duration = Duration::from_millis(80);
/// A Chromium app builds its accessibility tree only when the first read
/// arrives, and stays busy building it, so the press that wakes it reads
/// nothing. Once built, a read answers at once.
pub const DORMANT_TREE_RETRY_ATTEMPTS: u32 = 1;
pub const DORMANT_TREE_RETRY_DELAY: Duration = Duration::from_millis(300);
/// The picker reports the height its options need; a smaller one is a renderer
/// still laying out.
pub const MIN_PICKER_HEIGHT: f64 = 96.0;
/// How long an open waits for the renderer to size itself to the new payload
/// before showing anyway.
pub const SHOW_SIZE_WAIT: Duration = Duration::from_millis(80);
/// Matches the request id prefix `rewriteInTone()` tags local requests with.
pub const REQUEST_TAG: &str = "tone-rewrite-";

/// One selection capture (`selectionManager.captureSelectedText`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capture {
    Selected {
        session_id: String,
        text: String,
        character_count: usize,
    },
    /// Nothing selected.
    None,
    TooLarge {
        character_count: usize,
        max_characters: usize,
    },
    TargetChanged,
    Unavailable {
        code: String,
    },
}

/// What the picker renders (`ToneRewritePickerPayload` in `src/types/nativeApi.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state")]
pub enum PickerPayload {
    #[serde(rename = "ready", rename_all = "camelCase")]
    Ready {
        session_id: String,
        text: String,
        character_count: usize,
        selected_all: bool,
    },
    #[serde(rename = "error", rename_all = "camelCase")]
    Error {
        code: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_characters: Option<usize>,
    },
}

impl PickerPayload {
    pub fn error(code: &str) -> Self {
        PickerPayload::Error {
            code: code.to_string(),
            max_characters: None,
        }
    }

    pub fn state(&self) -> &'static str {
        match self {
            PickerPayload::Ready { .. } => "ready",
            PickerPayload::Error { .. } => "error",
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            PickerPayload::Error { code, .. } => Some(code),
            PickerPayload::Ready { .. } => None,
        }
    }
}

/// Maps a capture to what the picker shows. Only a selection carries text and
/// a session; everything else is a reason the picker explains.
pub fn to_picker_payload(capture: &Capture, selected_all: bool) -> PickerPayload {
    match capture {
        Capture::Selected {
            session_id,
            text,
            character_count,
        } => PickerPayload::Ready {
            session_id: session_id.clone(),
            text: text.clone(),
            character_count: *character_count,
            selected_all,
        },
        Capture::TooLarge { max_characters, .. } => PickerPayload::Error {
            code: "too_large".into(),
            max_characters: Some(*max_characters),
        },
        Capture::None => PickerPayload::error("nothing_selected"),
        Capture::TargetChanged => PickerPayload::error("target_changed"),
        Capture::Unavailable { code } if code.is_empty() => PickerPayload::error("unavailable"),
        Capture::Unavailable { code } => PickerPayload::error(code),
    }
}

/// A read the woken tree may still answer: nothing selected, or a synthetic
/// copy the busy app never processed.
pub fn is_retryable_capture(capture: &Capture) -> bool {
    match capture {
        Capture::None => true,
        Capture::Unavailable { code } => {
            matches!(
                code.as_str(),
                "accessibility_unavailable" | "selection_unreadable"
            )
        }
        _ => false,
    }
}

/// What the capture flow needs from the selection side.
pub trait CapturePort: Sync {
    /// The frontmost app at hotkey time.
    fn capture_target_pid(&self) -> impl Future<Output = Option<i32>> + Send;
    /// Whether this pid's accessibility tree is known to be dormant.
    fn is_dormant_tree(&self, pid: i32) -> bool;
    /// Reads the selection (confirming a "none"), opening a session on success.
    fn capture_selected_text(&self) -> impl Future<Output = Capture> + Send;
    /// ⌘A in the target. `Err` carries the decline code.
    fn select_all(&self) -> impl Future<Output = Result<(), String>> + Send;
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

/// Hotkey → the picker's payload: read the selection; a press that just woke a
/// dormant tree reads again; with `whole_field` on and nothing selected, select
/// everything in the field and read once more.
pub async fn capture_selection<P: CapturePort>(port: &P, whole_field: bool) -> PickerPayload {
    let pid = port.capture_target_pid().await;
    let dormant = |pid: Option<i32>| pid.is_some_and(|pid| port.is_dormant_tree(pid));
    let dormant_before = dormant(pid);
    let mut capture = port.capture_selected_text().await;
    if !matches!(capture, Capture::Selected { .. }) && !dormant_before && dormant(pid) {
        // This press woke the tree. Read again before ⌘A, which could still
        // land on the user's selection while the app catches up.
        let mut retries = 0;
        while retries < DORMANT_TREE_RETRY_ATTEMPTS && is_retryable_capture(&capture) {
            retries += 1;
            port.sleep(DORMANT_TREE_RETRY_DELAY).await;
            capture = port.capture_selected_text().await;
        }
        tracing::debug!(
            target: "tone-rewrite",
            ?pid,
            retries,
            status = capture_status(&capture),
            "Tone rewrite re-read a woken accessibility tree"
        );
    }
    if capture != Capture::None || !whole_field {
        return to_picker_payload(&capture, false);
    }

    if let Err(code) = port.select_all().await {
        tracing::debug!(target: "tone-rewrite", code, "Tone rewrite select-all declined");
        return to_picker_payload(&capture, false);
    }
    port.sleep(SELECT_ALL_SETTLE).await;
    capture = port.capture_selected_text().await;
    // Select-all came back empty: the field has no text.
    if capture == Capture::None {
        return PickerPayload::error("no_text");
    }
    to_picker_payload(&capture, true)
}

fn capture_status(capture: &Capture) -> &'static str {
    match capture {
        Capture::Selected { .. } => "selected",
        Capture::None => "none",
        Capture::TooLarge { .. } => "too_large",
        Capture::TargetChanged => "target_changed",
        Capture::Unavailable { .. } => "unavailable",
    }
}

// ---- Placement ---------------------------------------------------------------

/// A point in global screen coordinates (points, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// A rectangle in global screen coordinates (points, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }

    fn distance_squared(&self, point: Point) -> f64 {
        let dx = (self.x - point.x)
            .max(0.0)
            .max(point.x - (self.x + self.width));
        let dy = (self.y - point.y)
            .max(0.0)
            .max(point.y - (self.y + self.height));
        dx * dx + dy * dy
    }
}

/// One display: its full bounds and its work area (without the menu bar and
/// Dock).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Display {
    pub bounds: Rect,
    pub work_area: Rect,
}

/// The display containing `point`, else the closest one
/// (`screen.getDisplayNearestPoint`).
pub fn display_nearest_point(displays: &[Display], point: Point) -> Option<Display> {
    displays
        .iter()
        .find(|display| display.bounds.contains(point))
        .or_else(|| {
            displays.iter().min_by(|a, b| {
                a.bounds
                    .distance_squared(point)
                    .total_cmp(&b.bounds.distance_squared(point))
            })
        })
        .copied()
}

/// Keeps `bounds` inside the work area (`WindowPositionUtil.clampToWorkArea`):
/// the left/top edge wins when the window is larger than the area.
pub fn clamp_to_work_area(bounds: Rect, work_area: Rect) -> Point {
    Point {
        x: work_area
            .x
            .max(bounds.x.min(work_area.x + work_area.width - bounds.width)),
        y: work_area
            .y
            .max(bounds.y.min(work_area.y + work_area.height - bounds.height)),
    }
}

/// The picker's frame: `CURSOR_OFFSET` from the cursor it opened at, no taller
/// than the work area, clamped into it.
pub fn picker_bounds(cursor: Point, work_area: Rect, width: f64, height: f64) -> Rect {
    let fitted = height.min(work_area.height);
    let position = clamp_to_work_area(
        Rect {
            x: cursor.x + CURSOR_OFFSET,
            y: cursor.y + CURSOR_OFFSET,
            width,
            height: fitted,
        },
        work_area,
    );
    Rect {
        x: position.x,
        y: position.y,
        width,
        height: fitted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn selected(session_id: &str, text: &str) -> Capture {
        Capture::Selected {
            session_id: session_id.into(),
            text: text.into(),
            character_count: text.chars().count(),
        }
    }

    fn unavailable(code: &str) -> Capture {
        Capture::Unavailable { code: code.into() }
    }

    #[test]
    fn only_a_selection_hands_the_picker_text() {
        let to_json = |p: PickerPayload| serde_json::to_value(p).unwrap();
        assert_eq!(
            to_json(to_picker_payload(&selected("s1", "hey"), true)),
            json!({"state": "ready", "sessionId": "s1", "text": "hey", "characterCount": 3, "selectedAll": true})
        );
        assert_eq!(
            to_json(to_picker_payload(&Capture::None, false)),
            json!({"state": "error", "code": "nothing_selected"})
        );
        assert_eq!(
            to_json(to_picker_payload(
                &Capture::TooLarge {
                    character_count: 7000,
                    max_characters: 6000
                },
                false
            )),
            json!({"state": "error", "code": "too_large", "maxCharacters": 6000})
        );
        assert_eq!(
            to_json(to_picker_payload(&Capture::TargetChanged, false)),
            json!({"state": "error", "code": "target_changed"})
        );
        assert_eq!(
            to_json(to_picker_payload(&unavailable("modifiers_held"), false)),
            json!({"state": "error", "code": "modifiers_held"})
        );
        assert_eq!(
            to_json(to_picker_payload(&unavailable(""), false)),
            json!({"state": "error", "code": "unavailable"})
        );
    }

    #[test]
    fn retryable_captures_are_none_and_unread_copies() {
        assert!(is_retryable_capture(&Capture::None));
        assert!(is_retryable_capture(&unavailable("selection_unreadable")));
        assert!(is_retryable_capture(&unavailable(
            "accessibility_unavailable"
        )));
        assert!(!is_retryable_capture(&unavailable("modifiers_held")));
        assert!(!is_retryable_capture(&Capture::TargetChanged));
        assert!(!is_retryable_capture(&selected("s", "x")));
    }

    /// A selection side answering captures in order, repeating the last one.
    /// The first read is what finds the tree dormant.
    struct FakePort {
        captures: Mutex<VecDeque<Capture>>,
        last: Mutex<Capture>,
        dormant: AtomicBool,
        calls: Mutex<Vec<String>>,
    }

    impl FakePort {
        fn new(dormant_before: bool, captures: Vec<Capture>) -> Self {
            FakePort {
                last: Mutex::new(captures[0].clone()),
                captures: Mutex::new(captures.into()),
                dormant: AtomicBool::new(dormant_before),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CapturePort for FakePort {
        async fn capture_target_pid(&self) -> Option<i32> {
            Some(42)
        }

        fn is_dormant_tree(&self, _pid: i32) -> bool {
            self.dormant.load(Ordering::SeqCst)
        }

        async fn capture_selected_text(&self) -> Capture {
            self.calls.lock().unwrap().push("capture".into());
            self.dormant.store(true, Ordering::SeqCst);
            let mut last = self.last.lock().unwrap();
            if let Some(next) = self.captures.lock().unwrap().pop_front() {
                *last = next;
            }
            last.clone()
        }

        async fn select_all(&self) -> Result<(), String> {
            self.calls.lock().unwrap().push("selectAll".into());
            Ok(())
        }

        async fn sleep(&self, duration: Duration) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("sleep:{}", duration.as_millis()));
        }
    }

    fn without_sleeps(calls: Vec<String>) -> Vec<String> {
        calls
            .into_iter()
            .filter(|c| !c.starts_with("sleep"))
            .collect()
    }

    #[tokio::test]
    async fn the_press_that_wakes_a_dormant_tree_reads_again_before_selecting_all() {
        let port = FakePort::new(false, vec![Capture::None, selected("s1", "hey")]);
        let payload = capture_selection(&port, true).await;
        assert!(matches!(
            payload,
            PickerPayload::Ready {
                selected_all: false,
                ..
            }
        ));
        assert_eq!(port.calls(), ["capture", "sleep:300", "capture"]);
    }

    #[tokio::test]
    async fn an_unreadable_copy_on_the_waking_press_is_read_again() {
        let port = FakePort::new(
            false,
            vec![unavailable("selection_unreadable"), selected("s1", "hey")],
        );
        let payload = capture_selection(&port, true).await;
        assert_eq!(payload.state(), "ready");
        assert_eq!(without_sleeps(port.calls()), ["capture", "capture"]);
    }

    #[tokio::test]
    async fn a_tree_already_known_dormant_goes_straight_to_select_all() {
        let port = FakePort::new(true, vec![Capture::None, selected("s1", "all")]);
        let payload = capture_selection(&port, true).await;
        assert!(matches!(
            payload,
            PickerPayload::Ready {
                selected_all: true,
                ..
            }
        ));
        assert_eq!(
            port.calls(),
            ["capture", "selectAll", "sleep:80", "capture"]
        );
    }

    #[tokio::test]
    async fn select_all_runs_once_the_woken_trees_re_reads_are_spent() {
        let port = FakePort::new(false, vec![Capture::None]);
        let payload = capture_selection(&port, true).await;
        assert_eq!(payload, PickerPayload::error("no_text"));
        assert_eq!(
            without_sleeps(port.calls()),
            ["capture", "capture", "selectAll", "capture"]
        );
    }

    #[tokio::test]
    async fn nothing_selected_never_selects_all_while_whole_field_rewrite_is_off() {
        let port = FakePort::new(true, vec![Capture::None]);
        let payload = capture_selection(&port, false).await;
        assert_eq!(payload, PickerPayload::error("nothing_selected"));
        assert_eq!(port.calls(), ["capture"]);
    }

    #[tokio::test]
    async fn a_declined_select_all_reports_nothing_selected() {
        struct Declining(FakePort);
        impl CapturePort for Declining {
            async fn capture_target_pid(&self) -> Option<i32> {
                Some(42)
            }
            fn is_dormant_tree(&self, pid: i32) -> bool {
                self.0.is_dormant_tree(pid)
            }
            async fn capture_selected_text(&self) -> Capture {
                self.0.capture_selected_text().await
            }
            async fn select_all(&self) -> Result<(), String> {
                Err("terminal_target".into())
            }
            async fn sleep(&self, _: Duration) {}
        }
        let port = Declining(FakePort::new(true, vec![Capture::None]));
        assert_eq!(
            capture_selection(&port, true).await,
            PickerPayload::error("nothing_selected")
        );
    }

    #[tokio::test]
    async fn a_selection_never_triggers_select_all_or_a_re_read() {
        let port = FakePort::new(false, vec![selected("s1", "hey")]);
        assert_eq!(capture_selection(&port, true).await.state(), "ready");
        assert_eq!(port.calls(), ["capture"]);
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn the_picker_opens_eight_points_from_the_cursor() {
        let work_area = rect(0.0, 25.0, 1440.0, 875.0);
        assert_eq!(
            picker_bounds(Point { x: 100.0, y: 200.0 }, work_area, 280.0, 264.0),
            rect(108.0, 208.0, 280.0, 264.0)
        );
    }

    #[test]
    fn the_picker_is_clamped_into_the_work_area() {
        let work_area = rect(0.0, 25.0, 1440.0, 875.0);
        // Bottom-right corner: pushed up and left.
        assert_eq!(
            picker_bounds(
                Point {
                    x: 1430.0,
                    y: 890.0
                },
                work_area,
                280.0,
                300.0
            ),
            rect(1160.0, 600.0, 280.0, 300.0)
        );
        // Above the work area (the menu bar): pushed down.
        assert_eq!(
            picker_bounds(Point { x: 10.0, y: 0.0 }, work_area, 280.0, 100.0).y,
            25.0
        );
        // Taller than the work area: fitted to it.
        assert_eq!(
            picker_bounds(Point { x: 10.0, y: 500.0 }, work_area, 280.0, 2000.0),
            rect(18.0, 25.0, 280.0, 875.0)
        );
    }

    #[test]
    fn a_second_display_uses_its_own_work_area() {
        let main = Display {
            bounds: rect(0.0, 0.0, 1440.0, 900.0),
            work_area: rect(0.0, 25.0, 1440.0, 875.0),
        };
        let left = Display {
            bounds: rect(-1920.0, -180.0, 1920.0, 1080.0),
            work_area: rect(-1920.0, -155.0, 1920.0, 1055.0),
        };
        let displays = [main, left];
        let cursor = Point {
            x: -100.0,
            y: 850.0,
        };
        let display = display_nearest_point(&displays, cursor).unwrap();
        assert_eq!(display, left);
        assert_eq!(
            picker_bounds(cursor, display.work_area, 280.0, 264.0),
            rect(-280.0, 636.0, 280.0, 264.0)
        );
        // A point between displays goes to the closest one.
        assert_eq!(
            display_nearest_point(&displays, Point { x: 1500.0, y: 10.0 }),
            Some(main)
        );
        assert_eq!(display_nearest_point(&[], cursor), None);
    }
}
