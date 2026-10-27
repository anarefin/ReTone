//! Accelerator strings ("Control+Shift+Space", "Alt+num5") parsed into
//! modifiers plus a W3C `KeyboardEvent.code` name, which the app turns into a
//! global-shortcut key. The accelerator grammar on macOS: names
//! are case-insensitive, `Super`/`Meta`/`CommandOrControl` mean Command, and a
//! chord has exactly one non-modifier key.

use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct AcceleratorModifiers {
    pub command: bool,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Accelerator {
    pub modifiers: AcceleratorModifiers,
    /// W3C `KeyboardEvent.code` name ("KeyA", "F8", "Space").
    pub code: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceleratorError {
    Empty,
    EmptyToken,
    NoKey,
    MultipleKeys,
    UnknownKey(String),
}

impl fmt::Display for AcceleratorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty accelerator"),
            Self::EmptyToken => write!(f, "accelerator has an empty part"),
            Self::NoKey => write!(f, "accelerator has no key"),
            Self::MultipleKeys => write!(f, "accelerator has more than one key"),
            Self::UnknownKey(key) => write!(f, "unsupported key \"{key}\""),
        }
    }
}

impl std::error::Error for AcceleratorError {}

/// Accelerator key names (lowercased) → W3C code. `Plus` is the `=` key with Shift.
fn key_code(name: &str) -> Option<(&'static str, bool)> {
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "plus" => return Some(("Equal", true)),
        "`" | "backquote" => "Backquote",
        "-" | "minus" => "Minus",
        "=" | "equal" => "Equal",
        "[" | "bracketleft" => "BracketLeft",
        "]" | "bracketright" => "BracketRight",
        "\\" | "backslash" => "Backslash",
        ";" | "semicolon" => "Semicolon",
        "'" | "quote" => "Quote",
        "," | "comma" => "Comma",
        "." | "period" => "Period",
        "/" | "slash" => "Slash",
        "space" => "Space",
        "tab" => "Tab",
        "capslock" => "CapsLock",
        "numlock" => "NumLock",
        "scrolllock" => "ScrollLock",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "return" | "enter" => "Enter",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "escape" | "esc" => "Escape",
        "volumeup" => "AudioVolumeUp",
        "volumedown" => "AudioVolumeDown",
        "volumemute" => "AudioVolumeMute",
        "medianexttrack" => "MediaTrackNext",
        "mediaprevioustrack" => "MediaTrackPrevious",
        "mediastop" => "MediaStop",
        "mediaplaypause" => "MediaPlayPause",
        "printscreen" => "PrintScreen",
        "pause" => "Pause",
        "numdec" => "NumpadDecimal",
        "numadd" => "NumpadAdd",
        "numsub" => "NumpadSubtract",
        "nummult" => "NumpadMultiply",
        "numdiv" => "NumpadDivide",
        _ => return indexed_key(&lower).map(|code| (code, false)),
    };
    Some((code, false))
}

/// Letters, digits, F1–F24 and num0–num9.
fn indexed_key(lower: &str) -> Option<&'static str> {
    const LETTERS: [&str; 26] = [
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK",
        "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV",
        "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    const DIGITS: [&str; 10] = [
        "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
        "Digit9",
    ];
    const NUMPAD: [&str; 10] = [
        "Numpad0", "Numpad1", "Numpad2", "Numpad3", "Numpad4", "Numpad5", "Numpad6", "Numpad7",
        "Numpad8", "Numpad9",
    ];
    const FUNCTION: [&str; 24] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14",
        "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
    ];
    let bytes = lower.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0];
        if c.is_ascii_lowercase() {
            return Some(LETTERS[(c - b'a') as usize]);
        }
        if c.is_ascii_digit() {
            return Some(DIGITS[(c - b'0') as usize]);
        }
        return None;
    }
    if let Some(n) = lower.strip_prefix("num")
        && n.len() == 1
        && let Ok(index) = n.parse::<usize>()
    {
        return Some(NUMPAD[index]);
    }
    if let Some(n) = lower.strip_prefix('f')
        && !n.starts_with('0')
        && let Ok(index) = n.parse::<usize>()
        && (1..=24).contains(&index)
    {
        return Some(FUNCTION[index - 1]);
    }
    None
}

/// Applies a modifier name; false when `token` is not a modifier.
fn apply_modifier(modifiers: &mut AcceleratorModifiers, token: &str) -> bool {
    match token.to_ascii_lowercase().as_str() {
        "command" | "cmd" | "super" | "meta" | "commandorcontrol" | "cmdorctrl" => {
            modifiers.command = true
        }
        "control" | "ctrl" => modifiers.control = true,
        "alt" | "option" | "altgr" => modifiers.alt = true,
        "shift" => modifiers.shift = true,
        _ => return false,
    }
    true
}

/// Parses an accelerator string.
pub fn parse_accelerator(accelerator: &str) -> Result<Accelerator, AcceleratorError> {
    if accelerator.trim().is_empty() {
        return Err(AcceleratorError::Empty);
    }
    let tokens = split_tokens(accelerator)?;
    let mut modifiers = AcceleratorModifiers::default();
    let mut key: Option<&'static str> = None;
    for token in tokens {
        if apply_modifier(&mut modifiers, token) {
            continue;
        }
        if key.is_some() {
            return Err(AcceleratorError::MultipleKeys);
        }
        let (code, shifted) =
            key_code(token).ok_or_else(|| AcceleratorError::UnknownKey(token.to_string()))?;
        modifiers.shift |= shifted;
        key = Some(code);
    }
    Ok(Accelerator {
        modifiers,
        code: key.ok_or(AcceleratorError::NoKey)?,
    })
}

/// Splits on "+", keeping a trailing "+" key ("Control++" is Control and "+"),
/// read as the key named "+" — unsupported (the grammar spells it "Plus"), so
/// it surfaces as an unknown key.
fn split_tokens(accelerator: &str) -> Result<Vec<&str>, AcceleratorError> {
    let mut tokens = Vec::new();
    let mut rest = accelerator;
    loop {
        match rest.find('+') {
            Some(0) => {
                // "+" as the key itself: only valid as the final token.
                if rest.len() == 1 {
                    tokens.push("+");
                    break;
                }
                return Err(AcceleratorError::EmptyToken);
            }
            Some(index) => {
                let token = rest[..index].trim();
                if token.is_empty() {
                    return Err(AcceleratorError::EmptyToken);
                }
                tokens.push(token);
                rest = &rest[index + 1..];
                if rest.is_empty() {
                    return Err(AcceleratorError::EmptyToken);
                }
            }
            None => {
                let token = rest.trim();
                if token.is_empty() {
                    return Err(AcceleratorError::EmptyToken);
                }
                tokens.push(token);
                break;
            }
        }
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(command: bool, control: bool, alt: bool, shift: bool) -> AcceleratorModifiers {
        AcceleratorModifiers {
            command,
            control,
            alt,
            shift,
        }
    }

    #[test]
    fn parses_what_the_hotkey_input_records() {
        let a = parse_accelerator("Control+Shift+Space").unwrap();
        assert_eq!(a.modifiers, mods(false, true, false, true));
        assert_eq!(a.code, "Space");
        assert_eq!(parse_accelerator("F8").unwrap().code, "F8");
        assert_eq!(parse_accelerator("Command+Alt+K").unwrap().code, "KeyK");
        assert_eq!(parse_accelerator("Control+,").unwrap().code, "Comma");
        assert_eq!(parse_accelerator("Alt+`").unwrap().code, "Backquote");
        assert_eq!(parse_accelerator("Shift+\\").unwrap().code, "Backslash");
        assert_eq!(parse_accelerator("Command+Esc").unwrap().code, "Escape");
        assert_eq!(parse_accelerator("Up").unwrap().code, "ArrowUp");
        assert_eq!(parse_accelerator("Scrolllock").unwrap().code, "ScrollLock");
        assert_eq!(parse_accelerator("Control+num7").unwrap().code, "Numpad7");
        assert_eq!(parse_accelerator("numdec").unwrap().code, "NumpadDecimal");
        assert_eq!(parse_accelerator("nummult").unwrap().code, "NumpadMultiply");
        assert_eq!(parse_accelerator("F24").unwrap().code, "F24");
        assert_eq!(parse_accelerator("Command+3").unwrap().code, "Digit3");
        assert_eq!(
            parse_accelerator("MediaNextTrack").unwrap().code,
            "MediaTrackNext"
        );
    }

    #[test]
    fn macos_reads_super_meta_and_cmd_or_ctrl_as_command() {
        for spelling in [
            "Super+K",
            "Meta+K",
            "Cmd+K",
            "CommandOrControl+K",
            "CmdOrCtrl+K",
        ] {
            assert_eq!(
                parse_accelerator(spelling).unwrap().modifiers,
                mods(true, false, false, false),
                "{spelling}"
            );
        }
        assert_eq!(
            parse_accelerator("option+ctrl+a").unwrap().modifiers,
            mods(false, true, true, false)
        );
    }

    #[test]
    fn plus_is_the_equal_key_with_shift() {
        let a = parse_accelerator("Command+Plus").unwrap();
        assert_eq!(a.code, "Equal");
        assert!(a.modifiers.shift && a.modifiers.command);
    }

    #[test]
    fn rejects_chords_without_exactly_one_known_key() {
        assert_eq!(parse_accelerator(""), Err(AcceleratorError::Empty));
        assert_eq!(
            parse_accelerator("Control+Alt"),
            Err(AcceleratorError::NoKey)
        );
        assert_eq!(
            parse_accelerator("A+B"),
            Err(AcceleratorError::MultipleKeys)
        );
        assert_eq!(
            parse_accelerator("Control++K"),
            Err(AcceleratorError::EmptyToken)
        );
        assert!(matches!(
            parse_accelerator("LeftOption+K"),
            Err(AcceleratorError::UnknownKey(k)) if k == "LeftOption"
        ));
        assert!(matches!(
            parse_accelerator("GLOBE"),
            Err(AcceleratorError::UnknownKey(_))
        ));
        assert!(matches!(
            parse_accelerator("F25"),
            Err(AcceleratorError::UnknownKey(_))
        ));
        assert!(matches!(
            parse_accelerator("Control++"),
            Err(AcceleratorError::UnknownKey(_))
        ));
    }
}
