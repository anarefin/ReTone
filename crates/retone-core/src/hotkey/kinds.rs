//! Hotkey kinds: which hotkeys the native listener
//! serves and how the rest map onto a global-shortcut accelerator.

use regex::Regex;
use std::sync::LazyLock;

static RIGHT_SIDE_MODIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^Right(Control|Ctrl|Alt|Option|Shift|Command|Cmd|Super|Meta|Win)$")
        .expect("static regex")
});
static MOUSE_BUTTON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^MouseButton[45]$").expect("static regex"));
static FN_COMBINATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^Fn\+").expect("static regex"));

const MODIFIER_NAMES: &[&str] = &[
    "control",
    "ctrl",
    "alt",
    "option",
    "shift",
    "super",
    "meta",
    "win",
    "command",
    "cmd",
    "commandorcontrol",
    "cmdorctrl",
];

/// A lone right-side modifier ("RightCommand"); the native listener serves it.
pub fn is_right_side_modifier(hotkey: &str) -> bool {
    RIGHT_SIDE_MODIFIER.is_match(hotkey)
}

/// A chord of modifiers only ("Control+Alt").
pub fn is_modifier_only_hotkey(hotkey: &str) -> bool {
    hotkey.contains('+')
        && hotkey
            .split('+')
            .all(|part| MODIFIER_NAMES.contains(&part.to_lowercase().as_str()))
}

/// The Globe/Fn key on its own.
pub fn is_globe_like_hotkey(hotkey: &str) -> bool {
    hotkey == "GLOBE" || hotkey == "Fn"
}

/// "MouseButton4" or "MouseButton5".
pub fn is_mouse_button_hotkey(hotkey: &str) -> bool {
    MOUSE_BUTTON.is_match(hotkey)
}

/// Fn plus another key. A global shortcut cannot represent Fn, and registering
/// "Fn+A" as "A" would claim an ordinary typing key, so it is refused.
pub fn is_unsupported_fn_combination(hotkey: &str) -> bool {
    FN_COMBINATION.is_match(hotkey)
}

/// Hotkeys only the native listener (event tap) can see: Globe, mouse buttons
/// and lone right-side modifiers.
pub fn needs_native_listener(hotkey: &str) -> bool {
    is_globe_like_hotkey(hotkey) || is_mouse_button_hotkey(hotkey) || is_right_side_modifier(hotkey)
}

/// Right-side modifiers inside a chord become their plain modifier
/// ("RightCommand+K" → "Command+K"), as a global shortcut cannot tell sides.
pub fn normalize_to_accelerator(hotkey: &str) -> String {
    static RULES: LazyLock<[(Regex, &str); 4]> = LazyLock::new(|| {
        [
            (Regex::new(r"\bRight(Command|Cmd)\b").unwrap(), "Command"),
            (Regex::new(r"\bRight(Control|Ctrl)\b").unwrap(), "Control"),
            (Regex::new(r"\bRight(Alt|Option)\b").unwrap(), "Alt"),
            (Regex::new(r"\bRightShift\b").unwrap(), "Shift"),
        ]
    });
    RULES.iter().fold(hotkey.to_string(), |acc, (re, with)| {
        re.replace_all(&acc, *with).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_native_listener_hotkeys() {
        for hotkey in ["RightCommand", "rightoption", "RightShift", "RightCtrl"] {
            assert!(is_right_side_modifier(hotkey), "{hotkey}");
        }
        assert!(!is_right_side_modifier("RightCommand+K"));
        assert!(is_globe_like_hotkey("GLOBE") && is_globe_like_hotkey("Fn"));
        assert!(!is_globe_like_hotkey("Fn+A"));
        assert!(is_mouse_button_hotkey("MouseButton4") && is_mouse_button_hotkey("mousebutton5"));
        assert!(!is_mouse_button_hotkey("MouseButton3"));
        assert!(needs_native_listener("GLOBE") && !needs_native_listener("F8"));
    }

    #[test]
    fn classifies_modifier_only_and_fn_combinations() {
        assert!(is_modifier_only_hotkey("Control+Alt"));
        assert!(is_modifier_only_hotkey("CmdOrCtrl+Shift"));
        assert!(!is_modifier_only_hotkey("Control"));
        assert!(!is_modifier_only_hotkey("Control+K"));
        assert!(is_unsupported_fn_combination("Fn+A"));
        assert!(is_unsupported_fn_combination("fn+Space"));
        assert!(!is_unsupported_fn_combination("Fn"));
    }

    #[test]
    fn right_side_modifiers_in_a_chord_become_plain_modifiers() {
        assert_eq!(normalize_to_accelerator("RightCommand+K"), "Command+K");
        assert_eq!(
            normalize_to_accelerator("RightCtrl+RightOption+Space"),
            "Control+Alt+Space"
        );
        assert_eq!(normalize_to_accelerator("RightShift+F8"), "Shift+F8");
        assert_eq!(
            normalize_to_accelerator("Control+Shift+Space"),
            "Control+Shift+Space"
        );
    }
}
