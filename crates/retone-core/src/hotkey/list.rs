//! Hotkey lists: stored as comma-separated strings,
//! backward compatible with legacy single values. The comma KEY is itself a
//! valid hotkey (e.g. "Control+,"): no accelerator legitimately ends with "+",
//! so a split segment ending in "+" gets its comma restored.
//!
//! Keep in sync with the renderer twins in `src/utils/hotkeys.ts` and
//! `src/helpers/hotkeyList.js`.

const SEPARATOR: char = ',';

/// Normalizes a stored hotkey value into a clean list: trimmed, de-duplicated,
/// empties removed, order preserved.
pub fn parse_hotkey_list(value: &str) -> Vec<String> {
    normalize(value.split(SEPARATOR).collect())
}

/// [`parse_hotkey_list`] over several values (the JS array form); each item may
/// itself be a comma-separated list.
pub fn parse_hotkey_items<S: AsRef<str>>(items: &[S]) -> Vec<String> {
    normalize(
        items
            .iter()
            .flat_map(|item| item.as_ref().split(SEPARATOR))
            .collect(),
    )
}

fn normalize(raw: Vec<&str>) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for (i, segment) in raw.iter().enumerate() {
        let mut hotkey = segment.trim().to_string();
        // A segment ending in "+" lost its comma key to the split only if the
        // next segment is empty (the trailing comma key produced it).
        if hotkey.ends_with('+') && i + 1 < raw.len() && raw[i + 1].trim().is_empty() {
            hotkey.push(SEPARATOR);
        }
        if hotkey.is_empty() || result.contains(&hotkey) {
            continue;
        }
        result.push(hotkey);
    }
    result
}

/// The canonical comma-separated storage string.
pub fn serialize_hotkey_list(value: &str) -> String {
    parse_hotkey_list(value).join(",")
}

/// [`serialize_hotkey_list`] over several values.
pub fn serialize_hotkey_items<S: AsRef<str>>(items: &[S]) -> String {
    parse_hotkey_items(items).join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(value: &str) -> Vec<String> {
        parse_hotkey_list(value)
    }

    #[test]
    fn parses_legacy_single_hotkeys_and_lists() {
        assert_eq!(list("Control+Shift+R"), ["Control+Shift+R"]);
        assert_eq!(list("GLOBE"), ["GLOBE"]);
        assert_eq!(list("GLOBE,Control+Shift+R"), ["GLOBE", "Control+Shift+R"]);
        assert_eq!(
            list(" GLOBE , Control+Shift+R "),
            ["GLOBE", "Control+Shift+R"]
        );
    }

    #[test]
    fn drops_empty_entries_and_duplicates_preserving_order() {
        assert_eq!(list("F8,,F8,F9,"), ["F8", "F9"]);
        assert!(list("").is_empty());
    }

    #[test]
    fn accepts_items_including_nested_comma_strings() {
        assert_eq!(
            parse_hotkey_items(&["GLOBE", "Control+Shift+R"]),
            ["GLOBE", "Control+Shift+R"]
        );
        assert_eq!(
            parse_hotkey_items(&["GLOBE,Control+Alt", " F8 "]),
            ["GLOBE", "Control+Alt", "F8"]
        );
    }

    #[test]
    fn serializes_and_round_trips() {
        assert_eq!(
            serialize_hotkey_items(&["GLOBE", "Control+Shift+R"]),
            "GLOBE,Control+Shift+R"
        );
        assert_eq!(serialize_hotkey_list("F8, F9 ,F8"), "F8,F9");
        assert_eq!(serialize_hotkey_items::<&str>(&[]), "");
        let once = serialize_hotkey_list("GLOBE, Control+Shift+R ,MouseButton4");
        assert_eq!(once, "GLOBE,Control+Shift+R,MouseButton4");
        assert_eq!(list(&once), ["GLOBE", "Control+Shift+R", "MouseButton4"]);
    }

    #[test]
    fn keeps_comma_key_hotkeys_intact() {
        assert_eq!(list("Control+,"), ["Control+,"]);
        assert_eq!(list("Command+Shift+,"), ["Command+Shift+,"]);
        assert_eq!(list("GLOBE,Control+,"), ["GLOBE", "Control+,"]);
        assert_eq!(list("Control+,,F8"), ["Control+,", "F8"]);
        assert_eq!(
            parse_hotkey_items(&["Control+,", "F8"]),
            ["Control+,", "F8"]
        );
        let serialized = serialize_hotkey_items(&["Control+,", "F8", "GLOBE"]);
        assert_eq!(serialized, "Control+,,F8,GLOBE");
        assert_eq!(list(&serialized), ["Control+,", "F8", "GLOBE"]);
    }

    #[test]
    fn preserves_hotkeys_ending_with_plus() {
        assert_eq!(list("Control++,F8"), ["Control++", "F8"]);
        assert_eq!(
            parse_hotkey_items(&["Control++", "F8"]),
            ["Control++", "F8"]
        );
        assert_eq!(list("Control++,,F8"), ["Control++,", "F8"]);
        assert_eq!(list("Command++,Control+,,"), ["Command++", "Control+,"]);
    }
}
