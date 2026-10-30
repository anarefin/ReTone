# ReTone Development Notes

How to verify a change, how to check a release build, and the known limitations worth remembering. Architecture and conventions are in `CLAUDE.md`.

## Verify

```bash
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/node/bin:$PATH"
cargo build --workspace && cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
npm run typecheck && npm test && npm run i18n:check && npm run lint
```

Ignored smoke tests touch real OS state; run them by hand:

```bash
cargo test -p retone-macos --test smoke -- --ignored --nocapture        # clipboard round trip
cargo test -p retone-macos --lib smoke_starts -- --ignored              # real event tap
cargo test -p retone-llama --test smoke -- --ignored --nocapture        # real llama-server: download, completion, API-key 401/200, idle stop, quit, reap
```

CSP-enabled debug run (`tauri dev` loads the Vite server, where no CSP applies):

```bash
npx tauri build --debug --no-bundle && RETONE_CHANNEL=development RETONE_LOG_LEVEL=debug ./target/debug/retone
```

Expect `Settings page loaded url="tauri://localhost"` and no `Content-Security-Policy violation` or `Blocked untrusted navigation` lines.

## Release build

```bash
CI=true npm run build:mac                                              # → dist/ReTone.dmg
APP=target/aarch64-apple-darwin/release/bundle/macos/ReTone.app
codesign -dv --verbose=4 "$APP" && codesign --verify --deep --strict --verbose=2 "$APP"
RETONE_SMOKE_BIN_DIR="$PWD/$APP/Contents/Resources/bin" cargo test -p retone-llama --test smoke -- --ignored --nocapture
RETONE_CHANNEL=staging RETONE_LOG_LEVEL=debug "$APP/Contents/MacOS/retone"   # "Local inference ready … available=true binary=…/Resources/bin/…"
```

What a good bundle looks like:

- `codesign -dv`: `Identifier=com.arefin.retone`, `flags=0x10002(adhoc,runtime)`, entitlements = apple-events only; the deep strict verify reports "valid on disk" and "satisfies its Designated Requirement".
- `Contents/Resources/bin`: `llama-server-darwin-arm64` (`com.arefin.retone.llama-server`, `adhoc,runtime`, disable-library-validation) plus its dylibs, each passing `codesign --verify --strict`.
- `Contents/Resources/Assets.car` and `Contents/Resources/THIRD_PARTY_NOTICES.md` present; Info.plist has `NSAccessibilityUsageDescription`, `NSAppleEventsUsageDescription` and `CFBundleIconName = retone`.
- Tauri writes `ReTone_<version>_aarch64.dmg`; `postbuild:mac` (and CI) copy it to `dist/ReTone.dmg`, and the release asset is `ReTone-<version>-arm64.dmg` with a `.sha256` beside it.

## Cutting a release

1. Bump `version` in `package.json` (Tauri and Cargo read it from there) and commit to `main`.
2. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`. The tag must equal `v` + the `package.json` version.
3. `release.yml` runs the full CI pipeline, then **publishes** the release (not a draft) as Latest, with the DMG, its `.sha256`, the install notes and generated changes.
4. Check the release page, then download the DMG and run `shasum -a 256 -c ReTone-X.Y.Z-arm64.dmg.sha256`.
5. Settings › General › Updates in older builds finds the new release through `releases/latest` (once a day or on demand).

A bad release: delete it and its tag on GitHub (`gh release delete vX.Y.Z --cleanup-tag`), fix, bump the version and tag again. Don't reuse a version number.

## Manual checklist

Install `dist/ReTone.dmg` (drag to Applications; the first time, System Settings › Privacy & Security › Open Anyway on macOS 15+, right-click → Open on 12–14), grant Accessibility (re-grant after each build), then:

- [ ] Tray icon shows in the menu bar; its menu opens/hides Settings and quits
- [ ] Picker over a selection in TextEdit, a Safari/Chrome field, and an Electron-based app (Slack/VS Code)
- [ ] Every built-in option plus a custom option pastes in place; Esc and a second press close the picker
- [ ] Number keys, arrows and Enter work; only enabled options are shown
- [ ] Whole-field mode works (allow the System Events Automation prompt); Terminal/iTerm are declined
- [ ] Live preview streams for local/OpenAI/Groq/Gemini/Custom/OpenRouter; Anthropic shows the loading state
- [ ] Local model download from Settings, first load from the picker (log: `llama-server started successfully` with the `Contents/Resources/bin` binary), 5-minute idle unload, "Keep model loaded"
- [ ] Changing the selection mid-rewrite leaves the text untouched and copies the rewrite
- [ ] Accessibility warning in Settings while untrusted
- [ ] Globe/Fn and right-modifier hotkeys
- [ ] Launch at login: toggle on → System Settings › General › Login Items lists ReTone; switch it off there → the Settings toggle shows the requires-approval note and its button opens Login Items; log out/in → the app starts in the tray (log `Start minimized … launched_hidden=true`)
- [ ] Start minimized
- [ ] Quit from the tray while a local model is loaded → `pgrep llama-server` is empty; `kill -9` the app with a model loaded and relaunch → `Reaping stale sidecar` and no leftover process

## Known limitations

- **Ad-hoc signing**: every build is a new code identity. Accessibility, Automation, Input Monitoring and Keychain item ACLs must be re-granted after installing each build, and macOS may ask once per stored API key ("ReTone wants to use your confidential information").
- **Channels**: `tauri-plugin-single-instance` keys on the bundle id, so development/staging/production can't run side by side. All channels share one Keychain service, so they share keys.
- **Local DMG builds**: Tauri's `bundle_dmg.sh` lays out the DMG window by scripting Finder; without a Finder Automation grant it times out. `CI=true npm run build:mac` skips that step.
- **CSP** applies only to the custom protocol (`tauri build`, or `tauri build --debug --no-bundle`), not to `tauri dev`.
- **plugin-http** adds `Origin: tauri://localhost` (dev: `http://127.0.0.1:5183`) to renderer requests; a Custom server that rejects unknown origins will refuse them.
- **Keyboard layout**: the TIS layout lookup hops to the main queue and gives up after 1 s (`LayoutUnresolved`). Never block the main thread waiting for an input operation.
- **Pasteboard**: `NSPasteboard.stringForType` joins the text of every item; the restore check compares that joined text.
- **Globe action**: a SIGTERM/kill skips teardown, so a suppressed Globe action stays "Do Nothing" until the next launch restores it from `globe-preference-state.json`. A normal quit restores it.
- **Hotkey registration** hops to the main thread; calling `HotkeyService` mutators from a background thread would block on it. Under `tauri dev` the event tap needs the Accessibility grant of the terminal that launched the binary.
- **Picker**:
  - A capture lands on whatever is frontmost at the press, ReTone's own settings window included.
  - The first open of a freshly created picker can miss the 80 ms height wait and shows at 264 px until the renderer reports, then resizes.
  - Clicking into the picker may activate ReTone (an AppKit quirk when the non-activating mask is added after creation); apply re-activates the target first.
  - `NSApp.isActive` reads `true` while the non-activating panel is key even though the target stays frontmost; treat `app_active` in the log as informational.
- **llama-server**: `/health` and `/v1/models` stay unauthenticated in llama.cpp b9763; without the per-launch key nothing else is readable. The dev build copies `resources/bin` into `target/debug/bin` (~56 MB).
- **Dev noise**: a browser tab left open on `127.0.0.1:5183` also connects to the Vite server and logs `__TAURI_INTERNALS__` errors in the `tauri dev` output.
- Not tested on macOS 12.
