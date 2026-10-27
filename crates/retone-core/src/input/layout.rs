//! Keyboard-layout rules for the synthetic ⌘C/⌘V. The UCKeyTranslate calls live in
//! `retone-macos::keys`; these are the decisions around them.

/// Virtual key codes `UCKeyTranslate` is asked about.
pub const VIRTUAL_KEY_RANGE: std::ops::Range<u16> = 0..128;

/// Picks the layout to search. Non-ASCII layouts can define their own Command
/// shortcuts, so the current layout is used whenever it has layout data, even
/// if it lacks the character; the ASCII-capable layout only stands in when the
/// active input method has no layout data at all.
pub fn choose_layout<T>(
    current: Option<T>,
    ascii_capable: impl FnOnce() -> Option<T>,
) -> Option<T> {
    current.or_else(ascii_capable)
}

/// The first virtual key whose ⌘-modified output is exactly `character`.
/// `translate` returns what a key types, or `None` when translation fails.
///
/// Never falls back to the US-ANSI key code: on another layout that key can
/// trigger a different shortcut while still looking like a successful paste.
pub fn find_virtual_key(character: &str, translate: impl Fn(u16) -> Option<String>) -> Option<u16> {
    VIRTUAL_KEY_RANGE
        .into_iter()
        .find(|&key| translate(key).as_deref() == Some(character))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_layout_wins_even_without_the_character() {
        let chosen = choose_layout(Some("turkmen"), || Some("dvorak"));
        assert_eq!(chosen, Some("turkmen"));
    }

    #[test]
    fn ascii_layout_stands_in_only_without_current_layout_data() {
        assert_eq!(choose_layout(None, || Some("dvorak")), Some("dvorak"));
        assert_eq!(choose_layout::<&str>(None, || None), None);
    }

    #[test]
    fn finds_the_first_key_typing_the_character() {
        // Dvorak types "c" on key 34 and "v" on key 47.
        let dvorak = |key: u16| match key {
            34 => Some("c".to_string()),
            47 => Some("v".to_string()),
            _ => Some("x".to_string()),
        };
        assert_eq!(find_virtual_key("c", dvorak), Some(34));
        assert_eq!(find_virtual_key("v", dvorak), Some(47));
        assert_eq!(find_virtual_key("q", dvorak), None);
        assert_eq!(find_virtual_key("v", |_| None), None);
    }
}
