//! ⌘C/⌘V follow the active layout and
//! fall back to the ASCII-capable layout only when the current source has no
//! layout data. Uses the real installed layouts through UCKeyTranslate, without
//! switching the user's input source.
//!
//! `harness = false`: Text Input Source calls must run on the main thread, and
//! a custom `main` is the main thread.

#[cfg(target_os = "macos")]
fn main() {
    use retone_macos::keys::{InputSource, virtual_key_in};

    let source = |id: &str| {
        InputSource::with_id(id).unwrap_or_else(|| panic!("missing keyboard layout {id}"))
    };
    let dvorak = source("com.apple.keylayout.Dvorak");
    let lookup = |current: Option<&InputSource>, ascii: Option<&InputSource>, c: &str| {
        virtual_key_in(current, || ascii.cloned(), c)
    };

    let scenarios: [(&str, Option<&str>, u16, u16); 8] = [
        (
            "Russian after Dvorak",
            Some("com.apple.keylayout.Russian"),
            8,
            9,
        ),
        ("Dvorak", Some("com.apple.keylayout.Dvorak"), 34, 47),
        (
            "Dvorak QWERTY Command",
            Some("com.apple.keylayout.DVORAK-QWERTYCMD"),
            8,
            9,
        ),
        ("US", Some("com.apple.keylayout.US"), 8, 9),
        ("Colemak", Some("com.apple.keylayout.Colemak"), 8, 9),
        ("French", Some("com.apple.keylayout.French"), 8, 9),
        ("German", Some("com.apple.keylayout.German"), 8, 9),
        ("missing current source", None, 34, 47),
    ];
    let mut failures = Vec::new();
    for (name, id, copy_key, paste_key) in scenarios {
        let current = id.map(source);
        let copy = lookup(current.as_ref(), Some(&dvorak), "c");
        let paste = lookup(current.as_ref(), Some(&dvorak), "v");
        if copy != Some(copy_key) || paste != Some(paste_key) {
            failures.push(format!("wrong shortcuts for {name}: {copy:?}, {paste:?}"));
        }
    }

    // An input method without layout data falls back to the ASCII layout.
    let input_mode = InputSource::keyboard_input_modes()
        .into_iter()
        .find(|mode| mode.layout_data().is_none())
        .expect("an input mode without layout data");
    if lookup(Some(&input_mode), Some(&dvorak), "c") != Some(34)
        || lookup(Some(&input_mode), Some(&dvorak), "v") != Some(47)
    {
        failures.push("input method did not fall back to Dvorak".into());
    }

    if lookup(None, None, "v").is_some() {
        failures.push("no layout at all still resolved a key".into());
    }

    // A present layout without the shortcut fails safely instead of using another
    // layout.
    let turkmen = source("com.apple.keylayout.Turkmen");
    if lookup(Some(&turkmen), Some(&dvorak), "c").is_some()
        || lookup(Some(&turkmen), Some(&dvorak), "v").is_some()
    {
        failures.push("Turkmen resolved a shortcut it does not have".into());
    }

    // The live lookup runs inline on the main thread and finds some key.
    if retone_macos::keys::lookup_virtual_key("v").is_none() {
        eprintln!("note: the active layout has no ⌘V key (fine for this test)");
    }

    if failures.is_empty() {
        println!("keyboard_layout: layout scenarios passed");
    } else {
        for failure in &failures {
            eprintln!("keyboard_layout: {failure}");
        }
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {}
