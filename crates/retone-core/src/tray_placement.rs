//! macOS menu-bar placement for the tray icon.
//!
//! macOS saves a status item's position under its autosave name, so this
//! (lowercase) GUID must never change: changing it resets every user's
//! placement.

pub const MACOS_TRAY_GUID: &str = "28c35815-1466-4aae-bce6-abeaa0090e97";

/// The undocumented AppKit default holding the item's preferred position.
pub fn macos_tray_position_key() -> String {
    format!("NSStatusItem Preferred Position {MACOS_TRAY_GUID}")
}

/// Position 0 starts the icon as far right as macOS allows. It is registered as
/// a default, so once the user drags the icon their saved position wins.
pub const MACOS_TRAY_DEFAULT_POSITION: i64 = 0;

/// One step of tray creation, in order. The app crate performs these; the
/// order is part of the contract (the default must exist before the item does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayPlacementStep {
    RegisterDefault { key: String, position: i64 },
    CreateItem,
    SetAutosaveName(&'static str),
}

pub fn macos_tray_placement_plan() -> Vec<TrayPlacementStep> {
    vec![
        TrayPlacementStep::RegisterDefault {
            key: macos_tray_position_key(),
            position: MACOS_TRAY_DEFAULT_POSITION,
        },
        TrayPlacementStep::CreateItem,
        TrayPlacementStep::SetAutosaveName(MACOS_TRAY_GUID),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // The GUID is spelled out rather than referenced: a new GUID resets every user's tray position.
    #[test]
    fn registers_the_starting_position_before_creating_the_item_under_the_fixed_guid() {
        assert_eq!(
            macos_tray_placement_plan(),
            vec![
                TrayPlacementStep::RegisterDefault {
                    key: "NSStatusItem Preferred Position 28c35815-1466-4aae-bce6-abeaa0090e97"
                        .into(),
                    position: 0,
                },
                TrayPlacementStep::CreateItem,
                TrayPlacementStep::SetAutosaveName("28c35815-1466-4aae-bce6-abeaa0090e97"),
            ]
        );
    }

    #[test]
    fn the_guid_stays_lowercase() {
        assert_eq!(MACOS_TRAY_GUID, MACOS_TRAY_GUID.to_lowercase());
    }
}
