use super::*;
use retone_core::payload::Capture;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

/// Shared log of what the fakes saw, in order.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }
    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

#[derive(Default)]
struct FakeHost {
    log: Log,
    generation: AtomicU64,
    created: AtomicU64,
    visible: AtomicBool,
    bounds: Mutex<Option<Rect>>,
    shown: Mutex<Vec<PickerPayload>>,
}

impl FakeHost {
    fn visible(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }
    fn bounds(&self) -> Option<Rect> {
        *self.bounds.lock().unwrap()
    }
    fn windows_created(&self) -> u64 {
        self.created.load(Ordering::SeqCst)
    }
    /// The window died (the renderer crashed or it was closed).
    fn kill_window(&self) {
        self.generation.store(0, Ordering::SeqCst);
        self.visible.store(false, Ordering::SeqCst);
    }
}

impl PickerHost for FakeHost {
    async fn create(&self) -> u64 {
        let current = self.generation.load(Ordering::SeqCst);
        if current != 0 {
            return current;
        }
        let generation = self.created.fetch_add(1, Ordering::SeqCst) + 1;
        self.generation.store(generation, Ordering::SeqCst);
        self.log.push("create");
        generation
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    async fn wait_loaded(&self, _generation: u64) {}
    fn cursor_and_displays(&self) -> Option<(Point, Vec<Display>)> {
        let area = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 1000.0,
        };
        Some((
            Point { x: 0.0, y: 0.0 },
            vec![Display {
                bounds: area,
                work_area: area,
            }],
        ))
    }
    fn set_bounds(&self, bounds: Rect) {
        *self.bounds.lock().unwrap() = Some(bounds);
    }
    fn show_and_focus(&self) {
        self.visible.store(true, Ordering::SeqCst);
        self.log.push("show");
    }
    fn show_inactive(&self) {
        self.visible.store(true, Ordering::SeqCst);
        self.log.push("showInactive");
    }
    fn hide(&self) {
        self.visible.store(false, Ordering::SeqCst);
        self.log.push("hide");
    }
    fn destroy(&self) {
        self.kill_window();
        self.log.push("destroy");
    }
    fn emit_show(&self, payload: &PickerPayload) {
        self.shown.lock().unwrap().push(payload.clone());
        self.log.push("event:show");
    }
    fn emit_reset(&self) {
        self.log.push("event:reset");
    }
}

struct FakeSelection {
    log: Log,
    /// Whether the picker window is up when the target is activated.
    host_visible: Arc<dyn Fn() -> bool + Send + Sync>,
    replace_result: Mutex<Result<(), ReplaceError>>,
    capture: Mutex<Capture>,
    clipboard: Mutex<Option<String>>,
}

impl CapturePort for FakeSelection {
    async fn capture_target_pid(&self) -> Option<i32> {
        Some(42)
    }
    fn is_dormant_tree(&self, _pid: i32) -> bool {
        false
    }
    async fn capture_selected_text(&self) -> Capture {
        self.log.push("capture");
        self.capture.lock().unwrap().clone()
    }
    async fn select_all(&self) -> Result<(), String> {
        self.log.push("selectAll");
        Ok(())
    }
    async fn sleep(&self, _duration: Duration) {}
}

impl SelectionSide for FakeSelection {
    async fn activate_target(&self) {
        let shown = if (self.host_visible)() {
            "shown"
        } else {
            "hidden"
        };
        self.log.push(format!("activate:42:{shown}"));
    }
    async fn replace(&self, session_id: String, text: String) -> Result<(), ReplaceError> {
        self.log.push(format!("replace:{session_id}:{text}"));
        *self.replace_result.lock().unwrap()
    }
    fn write_clipboard(&self, text: &str) {
        *self.clipboard.lock().unwrap() = Some(text.to_string());
    }
    fn cancel_tagged(&self, prefix: &str) {
        self.log.push(format!("cancel:{prefix}"));
    }
}

type Controller = ToneRewriteController<Arc<FakeHost>, FakeSelection>;

// The controller owns its host; tests keep a second handle to inspect it.
impl PickerHost for Arc<FakeHost> {
    fn create(&self) -> impl Future<Output = u64> + Send {
        (**self).create()
    }
    fn generation(&self) -> u64 {
        (**self).generation()
    }
    fn wait_loaded(&self, generation: u64) -> impl Future<Output = ()> + Send {
        (**self).wait_loaded(generation)
    }
    fn cursor_and_displays(&self) -> Option<(Point, Vec<Display>)> {
        (**self).cursor_and_displays()
    }
    fn set_bounds(&self, bounds: Rect) {
        (**self).set_bounds(bounds)
    }
    fn show_and_focus(&self) {
        (**self).show_and_focus()
    }
    fn show_inactive(&self) {
        (**self).show_inactive()
    }
    fn hide(&self) {
        (**self).hide()
    }
    fn destroy(&self) {
        (**self).destroy()
    }
    fn emit_show(&self, payload: &PickerPayload) {
        (**self).emit_show(payload)
    }
    fn emit_reset(&self) {
        (**self).emit_reset()
    }
}

fn controller() -> (Arc<Controller>, Arc<FakeHost>, Log) {
    let log = Log::default();
    let host = Arc::new(FakeHost {
        log: log.clone(),
        ..FakeHost::default()
    });
    let watched = host.clone();
    let selection = FakeSelection {
        log: log.clone(),
        host_visible: Arc::new(move || watched.visible()),
        replace_result: Mutex::new(Ok(())),
        capture: Mutex::new(Capture::Selected {
            session_id: "s1".into(),
            text: "hey".into(),
            character_count: 3,
        }),
        clipboard: Mutex::new(None),
    };
    let controller = Arc::new(ToneRewriteController::new(host.clone(), selection));
    (controller, host, log)
}

fn ready() -> PickerPayload {
    PickerPayload::Ready {
        session_id: "s1".into(),
        text: "hey".into(),
        character_count: 3,
        selected_all: false,
    }
}

#[tokio::test(start_paused = true)]
async fn a_blur_cancels_a_local_rewrite_and_hides_the_window_for_reuse() {
    let (controller, host, log) = controller();
    controller.open_picker(ready()).await;
    assert_eq!(host.windows_created(), 1);
    assert!(host.visible());

    // Blur: another app took focus, nothing to hand back.
    assert!(controller.should_close_on_blur());
    log.clear();
    controller.close(false).await;
    assert_eq!(
        log.entries(),
        ["cancel:tone-rewrite-", "event:reset", "hide"]
    );
    assert!(!host.visible());
    assert_eq!(
        host.generation(),
        1,
        "the window is kept for the next press"
    );
    assert!(!controller.should_close_on_blur());
    assert_eq!(controller.payload(), None);
}

#[tokio::test(start_paused = true)]
async fn the_target_app_is_made_frontmost_before_the_picker_hides() {
    let (controller, host, log) = controller();
    controller.open_picker(ready()).await;
    log.clear();
    controller.close(true).await;
    assert_eq!(
        log.entries(),
        [
            "cancel:tone-rewrite-",
            "event:reset",
            "activate:42:shown",
            "hide"
        ]
    );
    assert!(!host.visible());

    controller.open_picker(ready()).await;
    log.clear();
    let result = controller.apply("s1".into(), "Hello.".into()).await;
    assert_eq!(result, ApplyResult::ok());
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::json!({"success": true})
    );
    let entries = log.entries();
    assert_eq!(entries[0], "activate:42:shown");
    assert_eq!(entries[1], "hide");
    assert_eq!(entries[2], "replace:s1:Hello.");
    // The successful apply closes without handing focus back again.
    assert_eq!(
        entries.iter().filter(|e| e.starts_with("activate")).count(),
        1
    );
    assert!(!host.visible());
    assert!(!controller.is_visible());
}

#[tokio::test(start_paused = true)]
async fn a_prewarmed_picker_is_shown_again_without_a_second_window() {
    let (controller, host, _log) = controller();
    controller.prewarm().await;
    assert_eq!(host.windows_created(), 1);
    assert!(!host.visible());

    controller.open_picker(ready()).await;
    assert_eq!(host.shown.lock().unwrap().last(), Some(&ready()));
    controller.close(true).await;

    let second = PickerPayload::error("nothing_selected");
    controller.open_picker(second.clone()).await;
    assert_eq!(host.windows_created(), 1);
    assert!(host.visible());
    assert_eq!(host.shown.lock().unwrap().last(), Some(&second));
    assert_eq!(controller.payload(), Some(second));
}

#[tokio::test(start_paused = true)]
async fn the_picker_shows_as_soon_as_the_renderer_reports_its_size() {
    let (controller, host, _log) = controller();
    controller.prewarm().await;
    let started = tokio::time::Instant::now();
    let opening = {
        let controller = controller.clone();
        tokio::spawn(async move { controller.open_picker(ready()).await })
    };
    // Let the open push its payload and start waiting.
    while host.shown.lock().unwrap().is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(!host.visible());
    controller.set_height(300.0);
    opening.await.unwrap();
    assert!(host.visible());
    assert_eq!(host.bounds().unwrap().height, 300.0);
    assert!(started.elapsed() < SHOW_SIZE_WAIT);
}

#[tokio::test(start_paused = true)]
async fn without_a_size_the_picker_shows_after_the_wait_at_its_initial_size() {
    let (controller, host, _log) = controller();
    let started = tokio::time::Instant::now();
    controller.open_picker(ready()).await;
    assert!(host.visible());
    assert_eq!(started.elapsed(), SHOW_SIZE_WAIT);
    assert_eq!(
        host.bounds(),
        Some(Rect {
            x: 8.0,
            y: 8.0,
            width: PICKER_WIDTH,
            height: PICKER_INITIAL_HEIGHT
        })
    );
}

#[tokio::test(start_paused = true)]
async fn heights_below_the_minimum_or_not_finite_are_ignored() {
    let (controller, host, _log) = controller();
    controller.open_picker(ready()).await;
    controller.set_height(50.0);
    controller.set_height(f64::NAN);
    assert_eq!(host.bounds().unwrap().height, PICKER_INITIAL_HEIGHT);
    controller.set_height(120.4);
    assert_eq!(host.bounds().unwrap().height, 121.0);
}

#[tokio::test(start_paused = true)]
async fn a_destroyed_picker_is_created_again_on_the_next_open() {
    let (controller, host, _log) = controller();
    controller.prewarm().await;
    host.kill_window();
    controller.window_gone();
    assert_eq!(host.generation(), 0);

    controller.open_picker(ready()).await;
    assert_eq!(host.windows_created(), 2);
    assert!(host.visible());
}

#[tokio::test(start_paused = true)]
async fn a_second_press_closes_the_open_picker() {
    let (controller, host, log) = controller();
    controller.trigger().await;
    assert!(host.visible());
    assert_eq!(controller.payload(), Some(ready()));
    assert_eq!(log.entries()[..2], ["capture", "create"]);

    log.clear();
    controller.trigger().await;
    assert!(!host.visible());
    assert!(!controller.is_visible());
    assert_eq!(
        log.entries(),
        [
            "cancel:tone-rewrite-",
            "event:reset",
            "activate:42:shown",
            "hide"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn whole_field_selects_all_only_once_the_setting_is_synced() {
    let (controller, _host, log) = controller();
    *controller.selection().capture.lock().unwrap() = Capture::None;
    assert!(!controller.whole_field());
    controller.trigger().await;
    assert_eq!(
        controller.payload(),
        Some(PickerPayload::error("nothing_selected"))
    );
    assert!(!log.entries().contains(&"selectAll".to_string()));

    controller.close(false).await;
    controller.set_whole_field(true);
    controller.trigger().await;
    assert!(log.entries().contains(&"selectAll".to_string()));
    assert_eq!(controller.payload(), Some(PickerPayload::error("no_text")));
}

#[tokio::test(start_paused = true)]
async fn a_declined_paste_copies_the_rewrite_and_shows_the_picker_again() {
    let (controller, host, log) = controller();
    *controller.selection().replace_result.lock().unwrap() = Err(ReplaceError::SelectionChanged);
    controller.open_picker(ready()).await;
    log.clear();

    let result = controller.apply("s1".into(), "Hello.".into()).await;
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::json!({"success": false, "code": "selection_changed", "copied": true})
    );
    assert_eq!(
        controller.selection().clipboard.lock().unwrap().as_deref(),
        Some("Hello.")
    );
    assert_eq!(
        log.entries(),
        [
            "activate:42:shown",
            "hide",
            "replace:s1:Hello.",
            "showInactive"
        ]
    );
    assert!(host.visible());
    // Still open: Esc, a blur or the hotkey closes it.
    assert!(controller.is_visible());
    assert!(controller.should_close_on_blur());
}

#[tokio::test(start_paused = true)]
async fn apply_refuses_a_closed_picker_and_an_empty_rewrite() {
    let (controller, _host, log) = controller();
    assert_eq!(
        controller.apply("s1".into(), "Hello.".into()).await,
        ApplyResult::failed("not_picker", false)
    );
    controller.open_picker(ready()).await;
    log.clear();
    let result = controller.apply("s1".into(), "  \n".into()).await;
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::json!({"success": false, "code": "invalid_replacement"})
    );
    assert!(log.entries().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_blur_during_an_apply_does_not_close_the_picker() {
    let (controller, _host, _log) = controller();
    controller.open_picker(ready()).await;
    controller.lock().applying = true;
    assert!(!controller.should_close_on_blur());
}

#[tokio::test(start_paused = true)]
async fn clearing_the_hotkey_destroys_the_window() {
    let (controller, host, log) = controller();
    controller.open_picker(ready()).await;
    controller.destroy();
    assert_eq!(host.generation(), 0);
    assert!(!controller.is_visible());
    assert!(log.entries().contains(&"destroy".to_string()));
    // A close after that only cancels requests.
    log.clear();
    controller.close(true).await;
    assert_eq!(log.entries(), ["cancel:tone-rewrite-"]);
}
