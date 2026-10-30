use super::*;
use crate::input::fakes::*;
use crate::input::ports::{PLAIN_TEXT_TYPE, PasteboardItem};
use std::sync::atomic::Ordering;

fn rich_snapshot() -> PasteboardSnapshot {
    PasteboardSnapshot {
        items: vec![
            PasteboardItem {
                representations: vec![
                    ("public.html".into(), b"<b>html before</b>".to_vec()),
                    ("public.rtf".into(), b"{\\rtf1 before}".to_vec()),
                    (PLAIN_TEXT_TYPE.into(), b"plain before".to_vec()),
                ],
            },
            PasteboardItem {
                representations: vec![("public.png".into(), vec![0x89, b'P', b'N', b'G'])],
            },
        ],
    }
}

fn text_snapshot(text: &str) -> PasteboardSnapshot {
    PasteboardSnapshot {
        items: vec![FakePasteboard::text_item(text)],
    }
}

// ---- clipboardRestore ports -------------------------------------------------

#[test]
fn restore_preserves_rich_clipboard_formats() {
    let h = Harness::new();
    h.pasteboard.set_contents(rich_snapshot());

    assert_eq!(
        h.layer.paste_text("rewritten", PasteOptions::default()),
        Ok(PasteOutcome::Pasted)
    );

    assert_eq!(h.pasteboard.contents(), rich_snapshot());
}

#[test]
fn restore_runs_when_the_clipboard_still_holds_the_pasted_text() {
    let h = Harness::new();
    h.pasteboard.external_write("previous clipboard");

    h.layer
        .paste_text("rewritten text", PasteOptions::default())
        .unwrap();

    assert_eq!(
        h.pasteboard.read_text().as_deref(),
        Some("previous clipboard")
    );
}

#[test]
fn restore_is_skipped_when_another_clipboard_write_wins_the_race() {
    let h = Harness::new();
    h.pasteboard.external_write("previous clipboard");
    let pasteboard = h.pasteboard.clone();
    h.clock.on_sleep(move |duration| {
        if duration == RESTORE_DELAY {
            pasteboard.external_write("user copied something else");
        }
    });

    h.layer
        .paste_text("rewritten text", PasteOptions::default())
        .unwrap();

    assert_eq!(
        h.pasteboard.read_text().as_deref(),
        Some("user copied something else")
    );
    assert_eq!(h.pasteboard.restores.load(Ordering::SeqCst), 0);
}

#[test]
fn restore_runs_when_only_the_change_count_moved_but_our_text_is_still_there() {
    let h = Harness::new();
    h.pasteboard.external_write("previous clipboard");
    let pasteboard = h.pasteboard.clone();
    h.clock.on_sleep(move |duration| {
        if duration == RESTORE_DELAY {
            // A clipboard manager re-publishes the same text.
            pasteboard.external_write("rewritten text");
        }
    });

    h.layer
        .paste_text("rewritten text", PasteOptions::default())
        .unwrap();

    assert_eq!(
        h.pasteboard.read_text().as_deref(),
        Some("previous clipboard")
    );
}

#[test]
fn fast_paste_restores_after_the_short_macos_delay() {
    let h = Harness::new();
    h.pasteboard.external_write("previous clipboard");

    h.layer
        .paste_text("rewritten text", PasteOptions::default())
        .unwrap();

    assert_eq!(h.keys.calls(), vec![KeyCall::Post(Shortcut::Paste)]);
    assert_eq!(h.clock.sleeps(), vec![PASTE_DELAY, RESTORE_DELAY]);
    assert_eq!(PASTE_DELAY, Duration::from_millis(120));
    assert_eq!(RESTORE_DELAY, Duration::from_millis(450));
}

#[test]
fn no_restore_when_not_asked() {
    let h = Harness::new();
    h.pasteboard.external_write("previous clipboard");

    h.layer
        .paste_text(
            "rewritten text",
            PasteOptions {
                restore_clipboard: false,
                ..PasteOptions::default()
            },
        )
        .unwrap();

    assert_eq!(h.pasteboard.read_text().as_deref(), Some("rewritten text"));
    assert_eq!(h.clock.sleeps(), vec![PASTE_DELAY]);
}

#[test]
fn an_unresolved_layout_leaves_the_text_on_the_clipboard() {
    let h = Harness::new();
    h.pasteboard.external_write("previous clipboard");
    h.keys.queue_posts([
        Err(KeySynthError::LayoutUnresolved),
        Err(KeySynthError::LayoutUnresolved),
    ]);

    let result = h
        .layer
        .paste_text("rewritten text", PasteOptions::default());

    assert_eq!(result, Err(PasteError::LayoutUnresolved));
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("could not resolve the active keyboard layout")
    );
    // Retried once, never through osascript (another layout's key could run a
    // different shortcut).
    assert_eq!(
        h.keys.calls(),
        vec![
            KeyCall::Post(Shortcut::Paste),
            KeyCall::Post(Shortcut::Paste)
        ]
    );
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("rewritten text"));
    assert_eq!(h.pasteboard.restores.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.clock.sleeps(),
        vec![PASTE_DELAY, PASTE_RETRY_DELAY, PASTE_DELAY]
    );
}

#[test]
fn an_untrusted_cgevent_paste_falls_back_to_osascript_and_restores() {
    let h = Harness::new();
    h.pasteboard.external_write("previous clipboard");
    h.keys.queue_posts([Err(KeySynthError::Untrusted)]);

    assert_eq!(
        h.layer
            .paste_text("rewritten text", PasteOptions::default()),
        Ok(PasteOutcome::Pasted)
    );

    assert_eq!(
        h.keys.calls(),
        vec![
            KeyCall::Post(Shortcut::Paste),
            KeyCall::Osascript(Shortcut::Paste)
        ]
    );
    assert_eq!(h.clock.sleeps(), vec![PASTE_DELAY, RESTORE_DELAY]);
    assert_eq!(
        h.pasteboard.read_text().as_deref(),
        Some("previous clipboard")
    );
}

#[test]
fn a_failed_fallback_is_retried_once_then_reported() {
    let h = Harness::new();
    h.keys.queue_posts([
        Err(KeySynthError::Failed("x".into())),
        Err(KeySynthError::Failed("x".into())),
    ]);
    h.keys.queue_osascript([
        Err(KeySynthError::Failed("osascript exit 1".into())),
        Err(KeySynthError::Failed("osascript exit 1".into())),
    ]);

    let error = h
        .layer
        .paste_text("text", PasteOptions::default())
        .unwrap_err();

    assert_eq!(error, PasteError::Failed("osascript exit 1".into()));
    assert!(error.clipboard_copied());
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("text"));
}

#[test]
fn a_paste_failure_drops_the_cached_accessibility_verdict() {
    let h = Harness::new();
    assert!(h.layer.check_accessibility(false));
    assert_eq!(h.accessibility.checks.load(Ordering::SeqCst), 1);
    h.keys.queue_posts([Err(KeySynthError::Untrusted)]);

    h.layer.paste_text("text", PasteOptions::default()).unwrap();
    h.layer.check_accessibility(false);

    // Paste's own check hit the cache; the one after the failure asked again.
    assert_eq!(h.accessibility.checks.load(Ordering::SeqCst), 2);
}

#[test]
fn macos_denial_preserves_text_and_shows_the_dialog_only_when_not_silent() {
    for silent in [true, false] {
        let h = Harness::new();
        h.accessibility.trusted.store(false, Ordering::SeqCst);
        h.pasteboard.external_write("previous");

        let error = h
            .layer
            .paste_text(
                "  final text\n",
                PasteOptions {
                    silent_accessibility_check: silent,
                    ..PasteOptions::default()
                },
            )
            .unwrap_err();

        assert_eq!(error, PasteError::AccessibilityRequired);
        assert_eq!(error.code(), Some("ACCESSIBILITY_PERMISSION_REQUIRED"));
        assert!(error.clipboard_copied());
        assert_eq!(h.pasteboard.read_text().as_deref(), Some("  final text\n"));
        assert_eq!(h.denials.load(Ordering::SeqCst), if silent { 0 } else { 1 });
        assert!(h.keys.calls().is_empty());
    }
}

#[test]
fn intentional_clipboard_fallback_stays_silent_and_succeeds() {
    let h = Harness::new();
    h.accessibility.trusted.store(false, Ordering::SeqCst);

    let result = h.layer.paste_text(
        "manual text",
        PasteOptions {
            allow_clipboard_fallback: true,
            ..PasteOptions::default()
        },
    );

    assert_eq!(result, Ok(PasteOutcome::ClipboardOnly));
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("manual text"));
    assert_eq!(h.denials.load(Ordering::SeqCst), 0);
}

#[test]
fn a_clipboard_write_failure_is_never_an_accessibility_denial() {
    let h = Harness::new();
    h.pasteboard.fail_writes.store(true, Ordering::SeqCst);

    let error = h
        .layer
        .paste_text("final text", PasteOptions::default())
        .unwrap_err();

    assert_eq!(error, PasteError::Clipboard("clipboard unavailable".into()));
    assert_eq!(error.code(), None);
    assert!(!error.clipboard_copied());
    assert_eq!(h.accessibility.checks.load(Ordering::SeqCst), 0);
}

// ---- Accessibility ----------------------------------------------------------

#[test]
fn a_non_silent_verdict_is_cached_for_five_seconds() {
    let h = Harness::new();
    h.accessibility.trusted.store(false, Ordering::SeqCst);

    assert!(!h.layer.check_accessibility(false));
    assert!(!h.layer.check_accessibility(false));
    assert_eq!(h.accessibility.checks.load(Ordering::SeqCst), 1);
    assert_eq!(h.denials.load(Ordering::SeqCst), 1);

    h.accessibility.trusted.store(true, Ordering::SeqCst);
    h.clock.advance(ACCESSIBILITY_CHECK_TTL);
    assert!(h.layer.check_accessibility(false));
    assert_eq!(h.accessibility.checks.load(Ordering::SeqCst), 2);
}

#[test]
fn a_silent_check_always_asks_and_never_shows_the_dialog() {
    let h = Harness::new();
    h.accessibility.trusted.store(false, Ordering::SeqCst);

    assert!(!h.layer.check_accessibility(true));
    assert!(!h.layer.check_accessibility(true));
    assert!(!h.layer.pre_warm_accessibility());

    assert_eq!(h.accessibility.checks.load(Ordering::SeqCst), 3);
    assert_eq!(h.denials.load(Ordering::SeqCst), 0);
}

#[test]
fn prompting_clears_the_cached_verdict() {
    let h = Harness::new();
    h.accessibility.trusted.store(false, Ordering::SeqCst);
    assert!(!h.layer.check_accessibility(false));
    h.accessibility.trusted.store(true, Ordering::SeqCst);

    assert!(h.layer.prompt_accessibility());
    assert!(h.layer.check_accessibility(false));
    assert_eq!(h.accessibility.prompts.load(Ordering::SeqCst), 1);
}

// ---- textEditMonitorCaptureTarget ports -------------------------------------

#[test]
fn concurrent_captures_share_one_frontmost_lookup() {
    let h = Arc::new(Harness::new());
    let (release, gate) = std::sync::mpsc::channel();
    *h.workspace.gate.lock().unwrap() = Some(gate);

    let first = {
        let h = h.clone();
        std::thread::spawn(move || h.layer.capture_target())
    };
    // Let the first capture take the lookup before the second arrives.
    while h.workspace.gate.lock().unwrap().is_some() {
        std::thread::yield_now();
    }
    let second = {
        let h = h.clone();
        std::thread::spawn(move || h.layer.capture_target())
    };
    release.send(()).unwrap();

    assert_eq!(first.join().unwrap(), Some(4242));
    assert_eq!(second.join().unwrap(), Some(4242));
    assert_eq!(h.workspace.frontmost_lookups.load(Ordering::SeqCst), 1);
    assert_eq!(h.layer.last_target_pid(), Some(4242));
}

#[test]
fn a_just_completed_capture_is_reused() {
    let h = Harness::new();
    assert_eq!(h.layer.capture_target(), Some(4242));
    h.clock.advance(Duration::from_millis(200));
    assert_eq!(h.layer.capture_target(), Some(4242));
    assert_eq!(h.workspace.frontmost_lookups.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_capture_is_retried_not_reused() {
    let h = Harness::with_frontmost(None);
    assert_eq!(h.layer.capture_target(), None);
    h.workspace
        .set_frontmost(Some(FakeWorkspace::app(4242, "TextEdit")));
    assert_eq!(h.layer.capture_target(), Some(4242));
    assert_eq!(h.workspace.frontmost_lookups.load(Ordering::SeqCst), 2);
}

#[test]
fn captures_refresh_once_the_reuse_window_has_passed() {
    let h = Harness::with_frontmost(Some(FakeWorkspace::app(1111, "Notes")));
    assert_eq!(h.layer.capture_target(), Some(1111));
    h.workspace
        .set_frontmost(Some(FakeWorkspace::app(2222, "Mail")));
    h.clock.advance(Duration::from_secs(10));
    assert_eq!(h.layer.capture_target(), Some(2222));
    assert_eq!(h.layer.last_target_pid(), Some(2222));
}

// ---- Activation -------------------------------------------------------------

#[test]
fn activating_the_frontmost_app_does_nothing() {
    let h = Harness::new();
    assert!(h.layer.activate_pid(4242));
    assert!(h.workspace.activations.lock().unwrap().is_empty());
    assert!(h.clock.sleeps().is_empty());
}

#[test]
fn activation_is_confirmed_by_polling_the_frontmost_app() {
    let h = Harness::new();
    h.workspace.add_app(FakeWorkspace::app(7, "Safari"));
    h.workspace.activation_lag.store(2, Ordering::SeqCst);

    assert!(h.layer.activate_pid(7));
    assert_eq!(*h.workspace.activations.lock().unwrap(), vec![7]);
    assert_eq!(h.clock.sleeps(), vec![ACTIVATE_CONFIRM_DELAY; 3]);
}

#[test]
fn activation_gives_up_after_six_checks() {
    let h = Harness::new();
    h.workspace.add_app(FakeWorkspace::app(7, "Safari"));
    h.workspace.activation_lag.store(100, Ordering::SeqCst);

    assert!(!h.layer.activate_pid(7));
    assert_eq!(h.clock.sleeps(), vec![Duration::from_millis(25); 6]);
}

// ---- Selection reads ----------------------------------------------------------

#[test]
fn an_accessibility_selection_is_read_without_touching_the_clipboard() {
    let h = Harness::new();
    h.pasteboard.external_write("user clipboard");
    h.layer.capture_target();
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Selected("hello".into()))]);

    let read = h.layer.read_selection(None, ReadOptions::default());

    assert_eq!(
        read,
        SelectionRead::Selected {
            text: "hello".into(),
            target: TargetApp {
                pid: 4242,
                name: None
            }
        }
    );
    assert!(h.keys.calls().is_empty());
    assert_eq!(h.pasteboard.change_count(), 1);
}

#[test]
fn no_target_is_unavailable_and_another_frontmost_app_is_a_changed_target() {
    let h = Harness::new();
    assert_eq!(
        h.layer.read_selection(None, ReadOptions::default()),
        SelectionRead::Unavailable {
            code: UnavailableCode::TargetUnavailable
        }
    );
    assert_eq!(
        h.layer.read_selection(Some(99), ReadOptions::default()),
        SelectionRead::TargetChanged
    );
}

#[test]
fn an_empty_selection_reads_as_none() {
    let h = Harness::new();
    h.reader.queue_native([NativeRead::Read(AxSelection::None)]);
    assert_eq!(
        h.layer.read_selection(Some(4242), ReadOptions::default()),
        SelectionRead::None {
            target: TargetApp {
                pid: 4242,
                name: None
            }
        }
    );
    assert!(h.keys.calls().is_empty());
}

#[test]
fn a_dormant_tree_falls_back_to_a_synthetic_copy_and_restores_the_clipboard() {
    let h = Harness::new();
    h.pasteboard.set_contents(rich_snapshot());
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Unknown)]);
    h.keys.set_copy_text(Some("chromium selection"));

    let read = h.layer.read_selection(Some(4242), ReadOptions::default());

    assert_eq!(
        read,
        SelectionRead::Selected {
            text: "chromium selection".into(),
            target: TargetApp {
                pid: 4242,
                name: Some("TextEdit".into())
            }
        }
    );
    assert_eq!(
        h.keys.calls(),
        vec![KeyCall::AwaitModifiers, KeyCall::Post(Shortcut::Copy)]
    );
    assert_eq!(h.pasteboard.contents(), rich_snapshot());
}

#[test]
fn a_persistent_no_value_failure_marks_the_pid_for_applescript() {
    let h = Harness::new();
    h.reader.queue_native([NativeRead::Failed {
        persistent_no_value: true,
    }]);
    h.reader.queue_applescript([
        AxSelection::Selected("via applescript".into()),
        AxSelection::Selected("again".into()),
    ]);

    assert_eq!(
        h.layer.read_ax_selection(4242),
        AxSelection::Selected("via applescript".into())
    );
    assert!(h.layer.is_native_selection_unsupported(4242));
    assert_eq!(
        h.layer.read_ax_selection(4242),
        AxSelection::Selected("again".into())
    );
    assert_eq!(h.reader.native_calls.load(Ordering::SeqCst), 1);
    assert_eq!(h.reader.applescript_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_timed_out_or_transient_failure_does_not_mark_the_pid() {
    let h = Harness::new();
    h.reader.queue_native([
        NativeRead::TimedOut,
        NativeRead::Failed {
            persistent_no_value: false,
        },
    ]);
    h.layer.read_ax_selection(4242);
    h.layer.read_ax_selection(4242);
    assert!(!h.layer.is_native_selection_unsupported(4242));
    assert_eq!(h.reader.applescript_calls.load(Ordering::SeqCst), 2);
}

#[test]
fn confirm_none_double_checks_a_dormant_app_with_a_copy() {
    let h = Harness::new();
    h.reader.queue_native([NativeRead::Failed {
        persistent_no_value: true,
    }]);
    h.reader.queue_applescript([
        AxSelection::Unavailable,
        AxSelection::None,
        AxSelection::None,
    ]);
    h.keys.set_copy_text(Some("woke up"));
    h.layer.read_ax_selection(4242);

    // Without confirm_none, AppleScript's "none" stands.
    assert!(matches!(
        h.layer.read_selection(Some(4242), ReadOptions::default()),
        SelectionRead::None { .. }
    ));
    let read = h.layer.read_selection(
        Some(4242),
        ReadOptions {
            confirm_none: true,
            ..ReadOptions::default()
        },
    );
    assert!(matches!(read, SelectionRead::Selected { ref text, .. } if text == "woke up"));
}

#[test]
fn a_copy_that_lands_nothing_reads_as_none_after_polling() {
    let h = Harness::new();
    h.pasteboard.external_write("user clipboard");
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Unknown)]);

    let read = h.layer.read_selection(Some(4242), ReadOptions::default());

    assert!(matches!(read, SelectionRead::None { .. }));
    let polls = h.clock.sleeps().len();
    assert_eq!(
        polls,
        (COPY_TIMEOUT.as_millis() / CLIPBOARD_POLL.as_millis()) as usize
    );
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("user clipboard"));
}

#[test]
fn a_copy_matching_the_old_clipboard_is_still_the_selection() {
    let h = Harness::new();
    h.pasteboard.external_write("same text");
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Unknown)]);
    h.keys.set_copy_text(Some("same text"));

    let read = h.layer.read_selection(Some(4242), ReadOptions::default());

    assert!(matches!(read, SelectionRead::Selected { ref text, .. } if text == "same text"));
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("same text"));
}

#[test]
fn copy_failures_map_to_their_codes_and_restore_the_clipboard() {
    type Setup = Box<dyn Fn(&Harness)>;
    let cases: Vec<(Setup, UnavailableCode)> = vec![
        (
            Box::new(|h: &Harness| h.accessibility.trusted.store(false, Ordering::SeqCst)),
            UnavailableCode::AccessibilityUnavailable,
        ),
        (
            Box::new(|h: &Harness| h.keys.modifiers_released.store(false, Ordering::SeqCst)),
            UnavailableCode::ModifiersHeld,
        ),
        (
            Box::new(|h: &Harness| h.keys.queue_posts([Err(KeySynthError::LayoutUnresolved)])),
            UnavailableCode::SelectionUnreadable,
        ),
        (
            Box::new(|h: &Harness| h.keys.queue_posts([Err(KeySynthError::Untrusted)])),
            UnavailableCode::AccessibilityUnavailable,
        ),
    ];
    for (setup, code) in cases {
        let h = Harness::new();
        h.pasteboard.set_contents(rich_snapshot());
        h.reader
            .queue_native([NativeRead::Read(AxSelection::Unknown)]);
        setup(&h);

        assert_eq!(
            h.layer.read_selection(Some(4242), ReadOptions::default()),
            SelectionRead::Unavailable { code }
        );
        assert_eq!(h.pasteboard.contents(), rich_snapshot(), "{code:?}");
    }
}

#[test]
fn a_copy_into_another_app_is_a_changed_target() {
    let h = Harness::new();
    h.pasteboard.external_write("user clipboard");
    let workspace = h.workspace.clone();
    // The user switches apps between the read and the copy.
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Unknown)]);
    let read = {
        workspace.set_frontmost(Some(FakeWorkspace::app(4242, "TextEdit")));
        h.layer.read_ax_selection(4242);
        workspace.set_frontmost(Some(FakeWorkspace::app(5, "Mail")));
        h.layer.capture_via_clipboard(4242)
    };
    assert_eq!(read, SelectionRead::TargetChanged);
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("user clipboard"));
}

#[test]
fn a_terminal_selection_reads_as_nothing_selected() {
    let h = Harness::with_frontmost(Some(FakeWorkspace::app(31, "Ghostty")));
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Unknown)]);
    h.keys.set_copy_text(Some("rm -rf build"));

    let read = h.layer.read_selection(Some(31), ReadOptions::default());

    assert!(matches!(read, SelectionRead::None { ref target } if target.pid == 31));
}

#[test]
fn a_line_copy_from_an_editor_reads_as_nothing_selected() {
    let h = Harness::with_frontmost(Some(FakeWorkspace::app(8, "Code")));
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Unknown)]);
    h.keys.set_copy_text(Some("let x = 1;\n"));
    assert!(matches!(
        h.layer.read_selection(Some(8), ReadOptions::default()),
        SelectionRead::None { .. }
    ));

    // The same text from an app that doesn't line-copy is a selection.
    let h = Harness::new();
    h.reader
        .queue_native([NativeRead::Read(AxSelection::Unknown)]);
    h.keys.set_copy_text(Some("let x = 1;\n"));
    assert!(matches!(
        h.layer.read_selection(Some(4242), ReadOptions::default()),
        SelectionRead::Selected { .. }
    ));
}

#[test]
fn restore_if_ours_keeps_a_clipboard_the_user_changed() {
    let h = Harness::new();
    let original = text_snapshot("old clipboard");
    let baseline: HashSet<String> = ["sentinel".to_string()].into();

    h.pasteboard.external_write("user's new copy");
    h.layer
        .restore_clipboard_if_ours(&original, &["sentinel"], &baseline);
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("user's new copy"));

    h.pasteboard.external_write("sentinel");
    h.layer
        .restore_clipboard_if_ours(&original, &["sentinel"], &baseline);
    assert_eq!(h.pasteboard.read_text().as_deref(), Some("old clipboard"));
}

#[test]
fn activation_failure_on_replace_reads_is_reported() {
    let h = Harness::new();
    h.workspace.add_app(FakeWorkspace::app(7, "Safari"));
    // Frontmost is unknown (picker focused, no app) so the read proceeds to
    // activation, which never lands.
    h.workspace.set_frontmost(None);
    h.workspace.activation_lag.store(100, Ordering::SeqCst);

    assert_eq!(
        h.layer.read_selection(
            Some(7),
            ReadOptions {
                activate: true,
                ..ReadOptions::default()
            }
        ),
        SelectionRead::Unavailable {
            code: UnavailableCode::ActivationFailed
        }
    );
}

// ---- Select all ---------------------------------------------------------------

#[test]
fn select_all_declines_terminals_and_changed_targets() {
    let h = Harness::with_frontmost(Some(FakeWorkspace::app(31, "iTerm")));
    assert_eq!(
        h.layer.select_all(Some(31)),
        Err(SelectAllError::TerminalTarget)
    );
    assert_eq!(
        h.layer.select_all(None),
        Err(SelectAllError::TargetUnavailable)
    );

    let h = Harness::new();
    h.workspace.add_app(FakeWorkspace::app(7, "Safari"));
    assert_eq!(
        h.layer.select_all(Some(7)),
        Err(SelectAllError::TargetChanged)
    );

    assert_eq!(h.layer.select_all(Some(4242)), Ok(()));
    assert_eq!(
        h.keys.calls(),
        vec![KeyCall::Osascript(Shortcut::SelectAll)]
    );

    h.keys
        .queue_osascript([Err(KeySynthError::Failed("x".into()))]);
    assert_eq!(
        h.layer.select_all(Some(4242)),
        Err(SelectAllError::SelectAllFailed)
    );
}

#[test]
fn sentinels_are_unique() {
    assert_ne!(new_sentinel(), new_sentinel());
    assert!(new_sentinel().starts_with("__RETONE_SELECTION_"));
}

#[test]
fn snapshot_plain_text_and_emptiness() {
    assert!(PasteboardSnapshot::default().is_empty());
    assert_eq!(text_snapshot("hi").plain_text().as_deref(), Some("hi"));
    assert_eq!(
        rich_snapshot().plain_text().as_deref(),
        Some("plain before")
    );
}
