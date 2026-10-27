//! App-name signatures: terminals (never select-all'd or pasted over, since a
//! pasted newline executes) and editors whose ⌘C on an empty selection copies
//! the whole line.

use std::sync::LazyLock;

/// Linux window classes (kept so the list matches the JS one) plus iTerm,
/// which only exists on macOS.
const TERMINAL_SIGNATURES: &[&str] = &[
    "konsole",
    "gnome-terminal",
    "terminal",
    "kitty",
    "alacritty",
    "terminator",
    "xterm",
    "urxvt",
    "rxvt",
    "tilix",
    "terminology",
    "wezterm",
    "foot",
    "st",
    "yakuake",
    "ghostty",
    "guake",
    "tilda",
    "hyper",
    "tabby",
    "sakura",
    "warp",
    "termius",
    "waveterm",
    "ptyxis",
    "kgx",
    "org.gnome.console",
    "cosmicterm",
    "xst",
    "stterm",
    "iterm",
];

/// Matched against the localized app name (and the bundle/executable names), so
/// the JetBrains IDEs need both spellings.
const LINE_COPY_EDITOR_SIGNATURES: &[&str] = &[
    "code",
    "cursor",
    "windsurf",
    "notepad++",
    "sublime",
    "jetbrains",
    "idea64",
    "intellij",
    "pycharm",
    "webstorm",
    "phpstorm",
    "rider64",
    "rider",
    "android studio",
    "clion",
    "goland",
    "rubymine",
    "datagrip",
    "dataspell",
    "studio64",
    "devenv",
];

static WORD_SPLIT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new("[^a-z0-9]+").expect("valid regex"));

/// Whether an app name (or window class) names a terminal. Short names like
/// "st" must be a whole word so they don't match inside unrelated names.
pub fn is_terminal_signature(signature: Option<&str>) -> bool {
    let Some(signature) = signature.filter(|s| !s.is_empty()) else {
        return false;
    };
    let normalized = signature.to_lowercase();
    let words: Vec<&str> = WORD_SPLIT.split(&normalized).collect();
    TERMINAL_SIGNATURES.iter().any(|term| {
        if term.len() > 2 {
            normalized.contains(term)
        } else {
            words.contains(term)
        }
    })
}

/// Whether any of the names belongs to an editor that copies the current line
/// when ⌘C lands on an empty selection.
pub fn is_line_copy_editor(names: &[Option<&str>]) -> bool {
    let signature = names
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    if signature.trim().is_empty() {
        return false;
    }
    LINE_COPY_EDITOR_SIGNATURES
        .iter()
        .any(|editor| signature.contains(editor))
}

/// Exactly one line with a trailing terminator: what an empty-selection line
/// copy produces.
pub fn looks_like_line_copy(text: &str) -> bool {
    let Some(body) = text.strip_suffix('\n') else {
        return false;
    };
    let body = body.strip_suffix('\r').unwrap_or(body);
    !body.contains('\n')
}

/// "<bundle name> <executable name>" from an executable path, e.g.
/// `/Applications/Visual Studio Code.app/Contents/MacOS/Code` →
/// "Visual Studio Code Code". Only these names are matched, so a directory
/// name can't read as a terminal.
pub fn target_names(executable_path: &str) -> String {
    let bundle = executable_path
        .split('/')
        .find_map(|part| part.strip_suffix(".app"))
        .filter(|_| executable_path.contains(".app/"))
        .unwrap_or("");
    let executable = executable_path.rsplit('/').next().unwrap_or("");
    format!("{bundle} {executable}").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_detection_matches_window_classes_and_macos_app_names() {
        for signature in ["konsole", "org.kde.konsole", "Ghostty", "kitty", "WezTerm"] {
            assert!(is_terminal_signature(Some(signature)), "{signature}");
        }
        assert!(is_terminal_signature(Some("iTerm2")));
        assert!(is_terminal_signature(Some("Terminal")));
        for signature in ["Dia", "Google Chrome", "Mail", ""] {
            assert!(!is_terminal_signature(Some(signature)), "{signature}");
        }
        assert!(!is_terminal_signature(None));
        assert!(is_terminal_signature(Some("st")));
        assert!(!is_terminal_signature(Some("com.system76.CosmicEdit")));
    }

    #[test]
    fn line_copy_editors_match_any_name() {
        assert!(is_line_copy_editor(&[None, Some("Code")]));
        assert!(is_line_copy_editor(&[Some("IntelliJ IDEA"), None]));
        assert!(is_line_copy_editor(&[Some("Android Studio")]));
        assert!(!is_line_copy_editor(&[Some("TextEdit"), Some("Safari")]));
        assert!(!is_line_copy_editor(&[None, None]));
    }

    #[test]
    fn line_copy_shape_is_one_terminated_line() {
        assert!(looks_like_line_copy("let x = 1;\n"));
        assert!(looks_like_line_copy("let x = 1;\r\n"));
        assert!(looks_like_line_copy("\n"));
        assert!(!looks_like_line_copy("let x = 1;"));
        assert!(!looks_like_line_copy("a\nb\n"));
    }

    #[test]
    fn target_names_use_bundle_and_executable_only() {
        assert_eq!(
            target_names("/Applications/Visual Studio Code.app/Contents/MacOS/Code"),
            "Visual Studio Code Code"
        );
        assert_eq!(
            target_names("/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal"),
            "Terminal Terminal"
        );
        assert_eq!(target_names("/usr/local/bin/kitty"), "kitty");
        assert!(!is_terminal_signature(Some(&target_names(
            "/Users/me/terminal-tools/Notes.app/Contents/MacOS/Notes"
        ))));
    }
}
