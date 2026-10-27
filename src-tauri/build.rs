use std::path::Path;

/// `bundle.resources` globs `resources/llama-bundle/*`, which only exists once
/// `npm run stage:llama-server` ran (`beforeBuildCommand` does, after
/// `download:llama-server`). A plain `cargo build`/`cargo test` on a fresh
/// clone gets a placeholder so the glob matches; such a build reports
/// llama-server as unavailable (debug builds also look in `resources/bin`).
fn ensure_resource_dir() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../resources/llama-bundle");
    let empty = std::fs::read_dir(&dir).map_or(true, |mut entries| entries.next().is_none());
    if empty {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            dir.join("README.txt"),
            "Staged llama-server goes here: run `npm run download:llama-server && npm run stage:llama-server`.\n",
        );
    }
}

fn main() {
    ensure_resource_dir();
    tauri_build::build()
}
