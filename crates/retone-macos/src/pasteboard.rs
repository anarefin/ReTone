//! The general pasteboard (`NSPasteboard`): plain-text read/write and a full
//! save/restore of every item and type.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};
use retone_core::input::ports::{Pasteboard, PasteboardError, PasteboardItem, PasteboardSnapshot};

fn general() -> Retained<NSPasteboard> {
    NSPasteboard::generalPasteboard()
}

fn string_type() -> &'static NSString {
    // SAFETY: an immutable NSString constant exported by AppKit.
    unsafe { NSPasteboardTypeString }
}

/// [`Pasteboard`] over `NSPasteboard.generalPasteboard`.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacPasteboard;

impl Pasteboard for MacPasteboard {
    fn change_count(&self) -> i64 {
        general().changeCount() as i64
    }

    fn read_text(&self) -> Option<String> {
        general()
            .stringForType(string_type())
            .map(|s| s.to_string())
    }

    fn write_text(&self, text: &str) -> Result<i64, PasteboardError> {
        let pasteboard = general();
        pasteboard.clearContents();
        if !pasteboard.setString_forType(&NSString::from_str(text), string_type()) {
            return Err(PasteboardError("NSPasteboard refused the text".into()));
        }
        Ok(pasteboard.changeCount() as i64)
    }

    fn save(&self) -> PasteboardSnapshot {
        let pasteboard = general();
        let items = pasteboard
            .pasteboardItems()
            .map(|items| {
                items
                    .iter()
                    .map(|item| PasteboardItem {
                        representations: item
                            .types()
                            .iter()
                            .filter_map(|kind| {
                                // A type whose data can't be produced (a lapsed
                                // promise) is left out.
                                item.dataForType(&kind)
                                    .map(|data| (kind.to_string(), data.to_vec()))
                            })
                            .collect(),
                    })
                    .filter(|item| !item.representations.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        PasteboardSnapshot { items }
    }

    fn restore(&self, snapshot: &PasteboardSnapshot) -> Result<i64, PasteboardError> {
        let pasteboard = general();
        pasteboard.clearContents();
        if snapshot.is_empty() {
            return Ok(pasteboard.changeCount() as i64);
        }
        let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = snapshot
            .items
            .iter()
            .filter(|item| !item.representations.is_empty())
            .map(|saved| {
                let item = NSPasteboardItem::new();
                for (kind, data) in &saved.representations {
                    item.setData_forType(&NSData::with_bytes(data), &NSString::from_str(kind));
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        let array = NSArray::from_retained_slice(&items);
        if !pasteboard.writeObjects(&array) {
            return Err(PasteboardError(
                "NSPasteboard refused the saved items".into(),
            ));
        }
        Ok(pasteboard.changeCount() as i64)
    }
}
