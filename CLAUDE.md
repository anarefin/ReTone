# ReTone Technical Reference for AI Assistants

This document describes the Rust/Tauri 2 ReTone codebase: a macOS (Apple silicon) tray app with one feature, **ReTone**. App id `com.arefin.retone`, repo `anarefin/retone`. ReTone is a standalone product: never reuse another app's identifiers (bundle id, userData/cache paths, Keychain service, env vars, tray GUIDs) and never touch other apps' data or settings. Third-party licenses are in `THIRD_PARTY_NOTICES.md` (bundled into the app); keep it current when adding bundled code.

`docs/DEVELOPMENT.md` holds the verification commands, release-build checks, manual checklist and known limitations. Read it before larger changes.

## Response Style

Keep responses focused, brief, and concise. Keep disclaimers and caveats short, and spend most of the response on the main answer. When asked to explain something, give a high-level summary unless an in-depth explanation is specifically requested.

## Project Overview

ReTone rewrites text selected in any other app. A global hotkey opens a small tone picker at the cursor; picking an option sends the selection to an LLM and pastes the rewrite back in place of the selection.

- Models: a local GGUF model run by a bundled llama.cpp `llama-server`, or bring-your-own-key providers (OpenAI, Anthropic, Gemini, Groq, OpenRouter, a Custom OpenAI-compatible endpoint).
- macOS arm64 only. Out of scope: accounts, a cloud service, history, onboarding, Windows/Linux, Vulkan.


## Core Technologies

- **Shell**: Tauri 2 (`tauri`, plugins `single-instance`, `global-shortcut`, `opener`, `http`), Rust stable (edition 2024), tokio, reqwest (rustls + `rustls-platform-verifier`), tracing, serde.
- **macOS**: `objc2` + `objc2-app-kit` / `-foundation` / `-core-graphics` / `-core-foundation` / `-service-management`, raw ApplicationServices FFI for AX, `keyring` 3 (apple-native).
- **Renderer**: React 19, TypeScript, Tailwind CSS v4, Vite, zustand, Radix primitives, react-i18next.
- **Local inference**: llama.cpp `llama-server` (pinned `b9763`, Metal), spawned as a sidecar.
- **Node.js 26** (`.nvmrc`, `engine-strict`). Never regenerate `package-lock.json` with another major version. Locally: `export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/node/bin:$PATH"`.

## Repo Layout

```
src/                     renderer (React); src/dist is the Vite output
src/platform/            nativeBridge.ts (window.retoneAPI over Tauri invoke/listen), providerFetch.ts
src-tauri/               app crate `retone`: setup, windows, tray, Dock, menu, commands, events
crates/retone-core/      pure logic, no OS deps, heavily unit-tested (OS behind traits)
crates/retone-macos/     AX, CGEvent, NSPasteboard, NSWorkspace, event tap, SMAppService, NSPanel
crates/retone-llama/     llama-server lifecycle, models, downloads, GGUF, context policy, sidecars
resources/mac/           entitlements (app and llama-server)
resources/bin/           downloaded llama.cpp build (gitignored)
resources/llama-bundle/  staged + signed llama-server closure that ships in the app (gitignored)
scripts/                 download-llama-server.js, stage-llama-server.js, check-i18n.js, sync-nucleo-icons.js
test/                    renderer tests (node --test)
```

## App Shape

- **Tray app.** Closing the settings window hides it; `ExitRequested` without an exit code is prevented; quitting is from the tray or app menu. `tauri-plugin-single-instance`: a second launch shows settings (it keys on the bundle id, so channels can't run side by side).
- **Settings window** (`windows.rs`, label `settings`, 760×760, min 560×480): shown after page load with a 3 s fallback; a load watchdog reloads after 1 s and shows `dialog.loadFailure.*` (packaged builds) if it still hasn't loaded 10 s later; a crashed renderer (`on_web_content_process_terminate`) reloads.
- **Picker window** (label `tone-rewrite-picker-<n>`, one label per generation): 280 px wide, frameless, transparent (`macOSPrivateApi`), all workspaces, level screen-saver + 1, content-protected, shadowless, created hidden. `retone-macos::panel` swaps its class to a non-activating `NSPanel` (`RetonePickerPanel`), so showing it makes it key without activating ReTone (macOS 14+ cooperative activation can refuse activation). Hidden and reused, never reloaded per press; destroyed only when the hotkey is cleared or the renderer crashes.
- Both windows load `index.html`; `AppRouter.jsx` renders the picker for `?tone-rewrite-picker=true`.
- **Dock** (`dock.rs` + `retone-core::dock_policy`): activation policy Regular/Accessory follows the settings window, driven only by explicit show/hide calls (never window events). Dock `Reopen` shows settings.
- **App icon** (`app_icon.rs` + `retone-core::app_icon`): Settings › Appearance offers `mono` (the bundle icon), `light`, `aurora`, `ring`, saved as `APP_ICON`. A choice replaces only the Dock image (`setApplicationIconImage`, re-applied each time the tile appears) and the tray template; Finder/Launchpad keep Mono, since rewriting the signed bundle would break the signature and the Accessibility grant.
- **Tray** (`tray.rs`): template icon (follows the app icon), Open/Hide Settings + Quit, rebuilt on visibility and language change; NSStatusItem `autosaveName` under ReTone's own GUID. Never change the GUID once released.
- **Menu** (`menu.rs`): app, Edit (so ⌘C/⌘V work in inputs), Window (+ Bring All to Front); View (reload/devtools) only in debug builds.
- **Channels**: `RETONE_CHANNEL` (`development`/`staging`/`production`). userData is `~/Library/Application Support/ReTone` in production, `ReTone-<channel>` otherwise.
- **Navigation guard** (`retone-core::navigation`): only the app's own page (`/`, `/index.html`, and the bare `tauri://localhost`) may load; http(s)/mailto links open in the browser via `tauri-plugin-opener`; a failed open shows `dialog.openLink.*`.
- **CSP** (`tauri.conf.json`): `default-src 'self'`, no remote `connect-src` (all renderer fetches go through plugin-http over IPC). The renderer logs `securitypolicyviolation` events to the app log. CSP applies to built bundles, not `tauri dev`.

## ReTone Flow (Rust names)

1. Hotkey (slot `toneRewrite`, saved as `TONE_REWRITE_KEY`) → `hotkeys.rs` (repeat gate, ignored while recording) → `tone_rewrite::toggle(app)` → `ToneRewriteController::trigger` (`src-tauri/src/tone_rewrite/controller.rs`). A press while the picker is open closes it; presses while an open/apply is in flight are dropped.
2. **Capture** (`retone-core::payload::capture_selection` over `TauriSelection`): `InputService::capture_target` (frontmost pid, 250 ms reuse), then `SessionStore::capture` reads the selection inside the clipboard lock (`confirm_none`). A Chromium/Electron app whose AX tree was dormant gets one re-read after 300 ms.
3. **Whole field**: with `toneRewriteWholeField` on and nothing selected, `select_all` (osascript ⌘A via System Events; terminals and changed targets declined), 80 ms settle, capture again. Empty → `no_text`.
4. **Payload** (`retone-core::payload::to_picker_payload`): `{state:"ready", sessionId, text, characterCount, selectedAll}` or `{state:"error", code}`: `nothing_selected`, `no_text`, `too_large` (over 6,000 code points), `target_changed`, `modifiers_held`, `accessibility_unavailable`, `unavailable`…
5. **Show** (`open_picker`): bounds 8 pt from the cursor clamped to the work area (`picker_bounds`), emit `tone-rewrite-show`, wait ≤ 80 ms for `tone_rewrite_set_height` (min 96), then show + make key (`TauriHost::show_and_focus`). The renderer also pulls the payload (`tone_rewrite_get_payload`).
6. **Pick**: arrows/hover, Enter or 1–9, Esc closes. A ready picker warms the local model (`warmToneRewriteModel` → `llama_server_start`).
7. **Rewrite** (renderer, `src/helpers/toneRewrite.ts`): `rewriteInTone` (one-shot via `ReasoningService.processText`) or, with live preview and a streamable provider (`local`, `openai`, `groq`, `gemini`, `custom`, `openrouter`; not Anthropic), `rewriteInToneStreaming`. The local stream goes straight to `http://127.0.0.1:<port>/v1/chat/completions` with `Authorization: Bearer <apiKey>` from `llama_server_start` (outside the bridge's busy lock). Output is cleaned by `cleanToneRewriteOutput`.
8. **Apply** (`tone_rewrite_apply` → `controller.apply`): activate the target, hide the picker, `SessionStore::replace` re-reads the selection and pastes only if unchanged (`selection_changed`, `target_changed`, `session_expired` (5-min TTL), `modifiers_held`, `paste_failed`…). On failure the rewrite goes to the clipboard and the picker re-shows inactive with `{success:false, code, copied:true}`.
9. **Close** (`controller.close`): clears state, cancels local requests tagged `tone-rewrite-`, emits `tone-rewrite-reset`, hides (never destroys). Blur closes without refocus unless applying. `prewarm` creates the picker 5 s after launch when a hotkey is saved and after one is set.

### Prompts and options

`src/config/prompts/toneRewrite.ts`: built-in tones `professional`, `casual`, `friendly`, `concise`, `fixGrammar`, `banglishToEnglish`, `banglishToBangla`; `toneRewriteOptions` normalized by `normalizeToneRewriteOptions`; 1–9 enabled; custom names ≤ 40 chars, prompts ≤ 2,000; `SHARED_RULES` always appended. Instructions stay English in every locale. Settings UI: `ReToneSection.tsx`, `ReToneOptionRow.tsx`.

## Inference

- **Scope** `toneRewrite` (`src/config/inferenceScopes.ts`), store keys `toneRewriteMode` (`local` | `providers`), `toneRewriteProvider`, `toneRewriteModel`, `toneRewriteCloudBaseUrl`, `toneRewriteDisableThinking`, `toneRewriteReasoningEffort`; resolved by `selectResolvedLLMConfig(state, "toneRewrite")` / `resolveToneRewriteInference()`.
- **Renderer providers**: `ReasoningService.ts` + `PROVIDER_REGISTRY` (`src/services/ai/inferenceProviders`). Every direct provider call goes through `src/platform/providerFetch.ts` (`@tauri-apps/plugin-http` fetch, runs in Rust, so no CORS; supports streaming; cancellations become `AbortError`). The plugin-http scope (`capabilities/default.json`) allows `https://*` and `http://*` (LAN Custom endpoints) and denies link-local `169.254.*`.
- **Anthropic** goes through Rust (`process_anthropic_reasoning`, `src-tauri/src/anthropic.rs`, 30 s deadline).
- **Connection check** (`test_provider_connection`): `retone-core::providers::connection_test` (12 s probe, failure priority, `unverified` for 408/429/5xx/timeouts) cached by `connection_cache` (10-min Connected TTL).
- **Errors**: `retone-core::providers::errors` is kept in sync with `src/helpers/providerHttpErrors.js` / `ipcErrorFields.js` (same codes and message keys; 500-char redacted detail). Raw provider bodies never reach UI text.
- **Model registry**: `src/models/modelRegistryData.json` is the single source of truth (renderer and `retone-llama::registry` via `include_str!`). Local families: Qwen and Gemma. Defaults: local mode, provider `qwen`, model `qwen3.5-4b-q4_k_m`; a saved local model the registry no longer lists is reset to that default at startup (`resetRetiredLocalModelSelections`).

### Local models (`crates/retone-llama`)

- `models::ModelManager`: list with download status, downloads (`{baseUrl}/{hfRepo}/resolve/main/{fileName}`, `.tmp` + Range resume, 3 retries, 30 s stall, 100 ms progress throttle, disk check ×1.2, ≥ 1 MB validation, optional MTP drafter), delete, context sizing (estimate → rung, preflight, grow once), `start_server_for` (→ `Endpoint { port, api_key }`), `prewarm`. Models live in `~/.cache/retone/models` (`RETONE_CACHE_ROOT` overrides) — `cache_dir`.
- `server::LlamaServer`: free port 8221–8240; args `--host 127.0.0.1 --ctx-size --jinja [--cache-ram] --n-gpu-layers 99 --no-slots --metrics --no-webui --alias local [--model-draft …]`; env `LLAMA_API_KEY` (random 64-hex key per launch), `DYLD_LIBRARY_PATH`, `LLAMA_ARG_FIT=off`; cwd temp dir; own process group. 120 s startup (500 ms `/health` polls), 5 s health check (not ready after 3 failures), grow-only context reuse, 5-min idle stop unless keep-resident (`set_keep_resident`), the idle stop first reads `/metrics` (`requests_processing` + `requests_deferred`) and re-arms while busy. Stop: SIGTERM → 5 s → SIGKILL.
- `client`: `ServerClient` trait (`ReqwestServerClient`, no proxy); every call but `/health` sends `Authorization: Bearer <key>`. `Endpoint`'s `Debug` redacts the key; never log it.
- `bridge::LocalReasoningBridge`: one request at a time (`LOCAL_MODEL_BUSY`), `cancel_tagged("tone-rewrite-")`, `<think>` stripping.
- `context` (context-size policy), `gguf` (v2/v3 reader with caps), `download`, `download_status`.
- `src-tauri/src/local.rs` `LocalInference`: binary dirs `Contents/Resources/bin` and, in debug builds, the repo's `resources/bin`.

### Sidecar rules (llama-server)

`llama-server` is the only sidecar. It is spawned in its own process group, writes its pid with `PidFile::write("llama", pid)` (`userData/sidecar-pids/llama.pid`) and clears it on exit, is reaped at startup by `reap_stale_sidecars` (only when `ps` shows `llama-server`; SIGTERM → 5 s → SIGKILL; every spawn waits for the reap via `spawn_gate`), and is stopped on quit through `SidecarRegistry::shutdown_all` (8 s deadline) from `ExitRequested`/`Exit`. It must listen on 127.0.0.1 only, with a per-launch API key passed by environment (argv is visible to every user through `ps`), and with `/slots` disabled (it echoes prompts, i.e. the user's text). Follow the same pattern for any new sidecar, plus a download + stage step wired into `beforeBuildCommand`.

## Selection Capture and Paste (`retone-core::input` + `retone-macos`)

- All clipboard work is serialized through `InputService::run_clipboard_operation` (tokio Mutex, `spawn_blocking`) in `src-tauri/src/clipboard.rs`. OS access sits behind traits (`Pasteboard`, `Accessibility`, `KeySynth`, `Workspace`, `SelectionReader`, `Clock`) so `InputLayer` logic is tested headless.
- **Read**: `retone-macos::ax` reads `AXSelectedText` of the focused element (1,600 ms on a worker thread; "unknown" for a dormant AX tree), System Events AppleScript fallback. Apps reporting unknown/unavailable fall back to a synthetic ⌘C: save every pasteboard item/type, write a sentinel, wait ≤ 1,000 ms for modifiers to be released (`modifiers_held`), post ⌘C, poll every 20 ms up to 1,200 ms, restore only while the clipboard still holds what we wrote. Never write AX attributes on target apps.
- **Paste**: save → write → Accessibility check → 120 ms → CGEvent ⌘V (`retone-macos::keys`, session tap, keycode from the current layout via `UCKeyTranslate` on the main queue; osascript fallback) → 450 ms → restore if `changeCount` unchanged or our text is still there.
- Terminals (`signatures`) are never select-all'd or overwritten; line-copy editors' one-line copies count as no selection.
- **Activation**: `NSRunningApplication.activateWithOptions`, 6×25 ms confirm, skipped if already frontmost.
- **Accessibility**: `AXIsProcessTrustedWithOptions`, cached 5 s; silent pre-warm at startup when a hotkey is saved; denial shows a localized osascript dialog with "Open" → Accessibility settings.

## Hotkeys (macOS)

- `retone-core::hotkey::manager::HotkeyManager` (named slots; only `toneRewrite`, no default): multi-hotkey lists (`list`), accelerator grammar (`accelerator`), atomic registration with rollback, `slot_conflict` / `already_registered` / `registration_failed` / `fn_combination_unsupported`, suggestions + `FALLBACK_HOTKEYS` (F8, F9, Control+Shift+Space), 600 ms `RepeatGate`.
- **Standard accelerators**: `tauri-plugin-global-shortcut` (`TauriShortcutBackend` in `src-tauri/src/hotkeys.rs`), fires on `Pressed`. Registration hops to the main thread; only mutate `HotkeyService` from the main thread (the mutating commands are sync).
- **Native listener** (`retone-macos::event_tap::MacEventTap` + `retone-core::hotkey::native`): listen-only CGEventTap on a dedicated CFRunLoop thread for Globe/Fn and right-side modifiers (`RightCommand`…), plus an active tap for mouse buttons 4/5 while configured; re-enabled on timeout. Started only when the slot needs it or while recording. Needs Accessibility/Input Monitoring; a failure shows a once-per-session dialog.
- **Globe action**: suppressed ("Do Nothing") while the listener owns Globe; `GlobePreference` writes `globe-preference-state.json` first and restores `com.apple.HIToolbox AppleFnUsageType` on stop, quit, or the next launch after a crash.
- Recording: `set_hotkey_listening_mode` suspends every binding and re-arms afterwards. `get_hotkey_mode_info` returns all-false (`isUsingGnome`… `linuxInputAccessDenied`).

## Settings Storage

- **Renderer localStorage** (per webview origin): `theme`, `uiLanguage`, `toneRewriteKey`, `toneRewriteMode`, `toneRewriteProvider`, `toneRewriteModel`, `toneRewriteCloudBaseUrl`, `toneRewriteDisableThinking` (default true), `toneRewriteReasoningEffort`, `toneRewriteOptions` (JSON), `toneRewriteLivePreview` (default true), `toneRewriteWholeField` (default false), `keepLocalModelLoaded` (default false). API keys never go to localStorage.
- **`userData/.env`** (`src-tauri/src/env.rs`, `retone-core::env_file`, line-preserving upserts): `TONE_REWRITE_KEY`, `START_MINIMIZED`, `UI_LANGUAGE`, `APP_ICON`, `LLAMA_GPU_BACKEND`, plus `RETONE_LOG_LEVEL` / `RETONE_CACHE_ROOT` if set by hand.
- `sync_startup_preferences` pushes `keepLocalModelLoaded` (→ `set_keep_resident`) and `toneRewriteWholeField` (→ controller) from the renderer.

## Secrets (Keychain)

`src-tauri/src/secrets.rs`: `keyring` service **`com.arefin.retone`**, one entry per env name: `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `GROQ_API_KEY`, `OPENROUTER_API_KEY`, `CUSTOM_CLEANUP_API_KEY`. Loaded on a background thread at startup (each loaded key emits `api-key-updated`); saves update memory then the Keychain; an empty save deletes. The Keychain ACL is tied to the code signature, so each new ad-hoc build may prompt once per stored key. All channels share the service. Tests use an in-memory `SecretBackend`.

## File Responsibilities (`src-tauri/src`)

- `main.rs` / `app.rs` — bootstrap (channel, userData, `.env`, log level, UI language), plugins, setup (windows, tray, menu, hotkeys, local inference, startup reap, Keychain load), run-event routing (picker blur/destroy, renderer crash, Dock reopen), teardown (hotkeys, Globe restore, sidecar shutdown). Startup failures show `startup.error.*` and exit 1 (`dialog.rs`).
- `lib.rs` — module list and `register_commands` (the one `generate_handler!` list) + `command_registry_tests`.
- `windows.rs`, `dock.rs`, `tray.rs`, `menu.rs`, `i18n.rs` (12 locales via `include_str!`, `{{var}}`, en fallback), `logging.rs`, `env.rs`, `secrets.rs`, `http.rs` (two reqwest clients: no redirects for hosted providers, ≤ 10 for Custom), `anthropic.rs`, `clipboard.rs` (`InputService`), `hotkeys.rs`, `local.rs`, `tone_rewrite/{mod,controller,host,selection}.rs`, `commands/*.rs` (one file per area).

## Commands and Events

The renderer keeps calling `window.retoneAPI.*`; `src/platform/nativeBridge.ts` maps each method to the snake_case command below (`invoke`) or event (`listen`). `getPlatform()` is synchronous (`"darwin"`).

| Area | Commands / events |
| --- | --- |
| Keys | `get_/save_{openai,anthropic,gemini,groq,openrouter,cleanup_custom}_key`, `test_provider_connection`; event `api-key-updated` |
| System | `get_app_version`, `open_external`, `write_clipboard`, `get_/set_auto_start_enabled`, `get_/set_start_minimized`, `get_/set_app_icon`, `get_/set_ui_language`, `sync_startup_preferences` |
| Permissions | `check_accessibility_permission`, `open_accessibility_settings`, `open_login_items_settings` |
| Hotkeys | `set_hotkey_listening_mode`, `get_hotkey_mode_info`; events `globe-key-pressed`, `globe-key-released`, `hotkey-registration-failed` |
| ReTone | `update_tone_rewrite_hotkey`, `get_tone_rewrite_key`, `tone_rewrite_get_payload`, `tone_rewrite_apply`, `tone_rewrite_close`, `tone_rewrite_set_height`; events `tone-rewrite-show`, `tone-rewrite-reset` |
| Inference | `process_local_reasoning`, `cancel_local_reasoning`, `check_local_reasoning_available`, `process_anthropic_reasoning` |
| Local models | `model_get_all`, `model_get_active_downloads`, `model_download`, `model_delete`, `model_cancel_download`, `llama_server_start` (`{success, port, apiKey}`), `llama_server_status` (includes `apiKey`), `llama_gpu_reset`; event `model-download-progress`. Vulkan commands (`detect_vulkan_gpu`, `get_llama_vulkan_status`, `download/cancel/delete_llama_vulkan_binary`) return "unsupported" |
| Logging | `get_log_level`, `app_log` |

A new command needs: the `#[tauri::command]` in `commands/<area>.rs`, an entry in `register_commands` (`lib.rs`), the bridge method in `src/platform/nativeBridge.ts` and its type in `src/types/nativeApi.ts`. Two drift tests enforce this: `command_registry_tests` (Rust: every `call("…")` in the bridge is registered and vice versa) and `test/platform/nativeBridge.test.js` (every `RetoneAPI` method is implemented by the bridge). Commands that answer the picker check the caller's window label (`not_picker` otherwise).

## Internationalization (i18n) — REQUIRED

All user-facing strings **must** go through react-i18next (renderer) or `src-tauri/src/i18n.rs` (`t`, `t_with`; tray, menu, dialogs, hotkey messages). Never hardcode UI text.

- Locales: `en`, `bn`, `es`, `fr`, `de`, `pt`, `it`, `ru`, `ja`, `zh-CN`, `zh-TW`, `ar` in `src/locales/<lang>/translation.json`, shared by both sides (Rust embeds them with `include_str!`). A new locale is registered in `src/locales/translations.ts`, `src/i18n.ts`, `src-tauri/src/i18n.rs` and the `SettingsWindow.tsx` language list. Arabic is RTL.
- Every new key goes into `en` and all other locale files; run `npm run i18n:check`. Provider error keys emitted by `retone-core::providers::errors` must exist in `en` (a Rust test checks).
- Keep `{{variable}}` interpolation. Don't translate brand names (ReTone), technical terms or format names. Model instructions in `src/config/prompts/toneRewrite.ts` stay English.
- The tray and menu are rebuilt on language change (`set_ui_language`).

## Image and Icon Assets — REQUIRED

- Raster UI assets in `src/assets/`; always `import` them (never literal paths). UI icons from `src/components/icons/` (vendored Nucleo; map in `nucleo-map.json`, run `node scripts/sync-nucleo-icons.js`); never import `lucide-react`.
- App icons: `src-tauri/icons/icon.icns` + `icon.png` (same art as `src/assets/icon.*`; must be RGBA or `generate_context!` fails); alternates in `src/assets/app-icons/`; the macOS 26 icon is `src/assets/Assets.car` (bundled as `Contents/Resources/Assets.car`, `CFBundleIconName = retone`). The tray uses `src/assets/iconTemplate@3x.png`. Brand art is rendered outside this repo; copy updated files in rather than editing PNGs by hand.
- Fonts: bundled Noto Sans + Noto Sans Bengali (`src/assets/fonts/`).

## Development

```bash
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/node/bin:$PATH"
npm install                 # Node 26
npm run tauri dev           # Vite on 127.0.0.1:5183 + debug app (predev downloads llama-server into resources/bin)
npm run build:mac           # tauri build --target aarch64-apple-darwin → .app + .dmg
cargo build --workspace && cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
npm run typecheck && npm test && npm run i18n:check && npm run lint
npm run download:llama-server   # resources/bin (pinned b9763, LLAMA_CPP_VERSION overrides; --force)
npm run stage:llama-server      # resources/llama-bundle: dylib closure + signing
npx tauri build --debug --no-bundle && RETONE_CHANNEL=development RETONE_LOG_LEVEL=debug ./target/debug/retone   # CSP-enabled debug run
```

Ignored smoke tests (touch real OS state; run by hand): `cargo test -p retone-macos --test smoke -- --ignored --nocapture` (clipboard round trip), `cargo test -p retone-macos --lib smoke_starts -- --ignored` (real event tap), `cargo test -p retone-llama --test smoke -- --ignored --nocapture` (real llama-server: download, completion, API-key 401/200, idle stop, quit, reap).

## Signing and Packaging

- `src-tauri/tauri.conf.json`: identifier `com.arefin.retone`, productName `ReTone`, targets `app` + `dmg`, `minimumSystemVersion` 12.0, `bundle.macOS.signingIdentity: "-"` (ad-hoc; **never null** — macOS then ignores the Accessibility grant), `hardenedRuntime: true`, `entitlements: resources/mac/entitlements.mac.plist`, `files: Resources/Assets.car`; `src-tauri/Info.plist` is merged (`NSAccessibilityUsageDescription`, `NSAppleEventsUsageDescription`, `CFBundleIconName`).
- **App entitlements**: only `com.apple.security.automation.apple-events` (the osascript → System Events paths). No JIT/unsigned-memory (WKWebView JITs out of process) and no library-validation exception (the app loads no third-party dylibs).
- **llama-server**: `beforeBuildCommand` = download → `stage-llama-server.js` → renderer build. Staging copies only the binary's `@rpath` closure (11 files, ~23 MB; `@rpath` = `@loader_path`, so it needs no `DYLD_LIBRARY_PATH`, which the hardened runtime would strip) into `resources/llama-bundle/`, signs every dylib ad-hoc and the binary ad-hoc with the hardened runtime, identifier `com.arefin.retone.llama-server` and `resources/mac/llama-server.entitlements.plist` (`disable-library-validation`: ad-hoc dylibs have no Team ID, so library validation would refuse them). `bundle.resources` maps it to `Contents/Resources/bin/`. `APPLE_SIGNING_IDENTITY` overrides `-` for both.
- `src-tauri/build.rs` writes a `README.txt` placeholder into an empty `resources/llama-bundle/` so a fresh-clone `cargo build`/`test` passes the resource glob.
- macOS 12: launch at login is unavailable (`SMAppService` is 13+; looked up at runtime with `AnyClass::get`, so the binary has no static reference to it). The toggle reads off and setting it returns an error.
- An ad-hoc grant (Accessibility, Input Monitoring, Automation, Keychain ACLs) is pinned to each build's hash: re-grant after installing every new build.
- **CI** (`.github/workflows/ci.yml`, macos-14): `npm ci`, typecheck, lint, `i18n:check`, `npm test`, `cargo fmt --check`, clippy `-D warnings`, `cargo test --workspace`, `npm run build:mac`, bundle signature checks, upload `ReTone.dmg`. `release.yml` on `v*` tags (tag must equal `package.json` version) reuses `ci.yml` and creates a **draft** release on `anarefin/retone`.

## Debugging

- `--log-level=debug` or `RETONE_LOG_LEVEL=debug` (environment or `userData/.env`). File logs only at debug: `userData/logs/debug-<timestamp>.log` (System Info header).
- Scopes (`tracing` targets): `app`, `window`, `dock`, `tray`, `menu`, `environment`, `i18n`, `ipc`, `hotkey`, `tone-rewrite`, `clipboard`, `reasoning`, `models`, `llama`, `sidecar`. Renderer logs arrive through `app_log`.
- Useful lines: `Picker focus phase=shown … key=true panel=true`, `Tone rewrite capture state=…`, `llama-server started successfully`, `llama-server idle timeout reached`, `Reaping stale sidecar`, `Content-Security-Policy violation`.

## Testing

- **Rust**: ~470 lib tests across the workspace. OS boundaries are traits with fakes (`input::fakes`, `test_support` in `retone-llama`, a mock HTTP server for reqwest paths); `crates/retone-macos/tests/keyboard_layout.rs` uses the real installed layouts.
- **JS**: `node --import tsx --test "test/**/*.test.js"` (renderer suites, the bridge drift test, `providerFetch`, the local-stream auth test).

### Manual checklist (built app, Accessibility granted)

- [ ] Hotkey opens the picker over a selection in TextEdit, Safari/Chrome text fields, and an Electron-based app (Slack/VS Code)
- [ ] Each built-in option and a custom option rewrites and pastes in place; Esc and a second press close the picker
- [ ] Number keys, arrows and Enter work; only enabled options are shown
- [ ] "Rewrite whole field" selects an unselected field; Terminal/iTerm are declined
- [ ] Live preview streams for local/OpenAI/Groq/Gemini/Custom/OpenRouter; Anthropic shows the loading state
- [ ] Local model download, first-use load, idle unload after 5 minutes, "Keep model loaded"
- [ ] Changing the selection before the rewrite returns leaves the text untouched and copies the rewrite
- [ ] Accessibility warning when untrusted
- [ ] Globe/Fn and right-modifier hotkeys
- [ ] Launch at login (including the requires-approval state), start minimized
- [ ] Quit kills llama-server; a stale llama-server is reaped on next launch

## Common Issues

1. **Picker says nothing is selected**: Chromium apps may need a second press while their AX tree wakes; check Accessibility (re-grant after every ad-hoc build).
2. **Rewrite copied instead of pasted**: the selection or focus changed, or keys were still held (`modifiers_held`).
3. **Hotkey doesn't fire**: conflict with another app or the OS; Globe/right-modifier/mouse hotkeys need the event tap (Accessibility/Input Monitoring for the app, or for the terminal under `tauri dev`).
4. **Whole field does nothing**: the Automation prompt for System Events was denied (System Settings › Privacy & Security › Automation).
5. **Local model fails**: llama-server missing (`npm run download:llama-server`; bundles need `stage:llama-server`), model file missing (re-download), out of memory (smaller model). Requests to the server without the per-launch key get 401.
6. **Keychain prompts on every build**: expected with ad-hoc signatures.
7. **Lockfile**: always `npm install` with Node 26.
