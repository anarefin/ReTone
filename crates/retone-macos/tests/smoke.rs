//! Real-machine smoke tests. Ignored by default because they touch the user's
//! clipboard (and put it back). Run once with:
//! `cargo test -p retone-macos --test smoke -- --ignored --nocapture`.
//! They never synthesize keystrokes.

#![cfg(target_os = "macos")]

use retone_core::input::ports::{Pasteboard, PasteboardItem, PasteboardSnapshot};
use retone_macos::pasteboard::MacPasteboard;

#[test]
#[ignore = "touches the real clipboard"]
fn pasteboard_save_and_restore_round_trip() {
    let board = MacPasteboard;
    let original = board.save();
    let original_count = board.change_count();
    println!(
        "saved {} item(s): {:?}",
        original.items.len(),
        original
            .items
            .iter()
            .map(|i| i
                .representations
                .iter()
                .map(|(t, d)| format!("{t} ({} B)", d.len()))
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );

    let result = std::panic::catch_unwind(|| {
        let count = board.write_text("retone smoke test").expect("write text");
        assert!(count > original_count);
        assert_eq!(board.read_text().as_deref(), Some("retone smoke test"));

        // A multi-item, multi-type snapshot survives a round trip.
        let rich = PasteboardSnapshot {
            items: vec![
                PasteboardItem {
                    representations: vec![
                        ("public.utf8-plain-text".into(), b"plain".to_vec()),
                        ("public.html".into(), b"<b>bold</b>".to_vec()),
                    ],
                },
                PasteboardItem {
                    representations: vec![("public.utf8-plain-text".into(), b"second".to_vec())],
                },
            ],
        };
        board.restore(&rich).expect("restore rich");
        assert_eq!(board.save(), rich);
        // stringForType joins every item's text.
        assert_eq!(board.read_text().as_deref(), Some("plain\nsecond"));
    });

    // Always put the user's clipboard back.
    board
        .restore(&original)
        .expect("restore the user's clipboard");
    let restored = board.save();
    println!("restored {} item(s)", restored.items.len());
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert_eq!(restored, original);
}

#[test]
#[ignore = "reads live system state"]
fn trust_check_and_frontmost_app_answer() {
    println!("AXIsProcessTrusted = {}", retone_macos::ax::is_trusted());
    println!("frontmost = {:?}", retone_macos::workspace::frontmost_app());
    println!("modifiers held = {}", retone_macos::keys::modifiers_held());
}
