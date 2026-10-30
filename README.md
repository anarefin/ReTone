# ReTone

ReTone is a macOS menu-bar (tray) app that rewrites text you've selected in any other app. Press a global hotkey and a small tone picker opens at the cursor. Pick a tone (Professional, Casual, Concise, Fix Grammar…) and ReTone sends the selection to an LLM, then pastes the rewrite back over the selection.

The model can be a local GGUF model, run by a bundled llama.cpp `llama-server`, or your own API key for OpenAI, Anthropic, Gemini, Groq, OpenRouter or a custom OpenAI-compatible endpoint.

Built with Tauri 2 (Rust) and React. **macOS on Apple silicon (arm64) only.**

## Prerequisites

- macOS 12 or later on an Apple silicon Mac
- Xcode Command Line Tools: `xcode-select --install`
- Rust stable, installed with [rustup](https://rustup.rs)
- Node.js **26** (LTS). `.nvmrc` pins it and `engine-strict` refuses other versions. For example: `brew install node`

## Run in development

1. **Clone the repo**

   ```bash
   git clone https://github.com/anarefin/retone.git
   cd retone
   ```

2. **Put Cargo and Node 26 on your PATH**

   ```bash
   export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/node/bin:$PATH"
   ```

3. **Install dependencies**

   ```bash
   npm install
   ```

   Only use Node 26 here. Another major version would regenerate `package-lock.json`.

4. **Start the app**

   ```bash
   npm run tauri dev
   ```

   The first run downloads the pinned llama.cpp `llama-server` into `resources/bin`. It then starts Vite on `127.0.0.1:5183` and launches the debug app. The Settings window opens and the ReTone icon appears in the menu bar.

5. **Grant Accessibility**

   Go to System Settings › Privacy & Security › Accessibility. Under `tauri dev`, grant it to the terminal app that launched the command. ReTone needs it to read the selection and paste.

6. **Configure ReTone in Settings**
   - Set a ReTone hotkey.
   - Choose a model. You can download the default local model (Qwen 3.5 4B) or switch to a provider and enter your API key. Keys are stored in the macOS Keychain.

7. **Try it**

   Select some text in any app (TextEdit, Safari, Slack…) and press your hotkey. Then pick a tone with the mouse, the arrow keys + Enter, or a number key 1–9. Press Esc to close the picker.

## Build the app

```bash
CI=true npm run build:mac      # → dist/ReTone.dmg
```

`CI=true` skips the Finder-scripted DMG window layout, which otherwise needs a Finder Automation grant.

To install, open the DMG and drag ReTone to Applications. The first time, right-click it and choose Open. Builds are signed ad-hoc, so you have to grant Accessibility again after installing each new build.

## Tests and checks

```bash
cargo build --workspace && cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check
npm run typecheck && npm test && npm run i18n:check && npm run lint
```

## Troubleshooting

- **"Nothing selected"**: check the Accessibility grant (grant it again after every new build). In Chromium/Electron apps, press the hotkey a second time.
- **Local model fails to start**: run `npm run download:llama-server`, or delete and re-download the model from Settings.
- **Keychain prompts after each build**: this is expected with ad-hoc signing.
- **Debug logs**: run with `RETONE_LOG_LEVEL=debug`. Logs go to `~/Library/Application Support/ReTone/logs/` for production builds and to `ReTone-<channel>/logs/` for other channels.

## More

- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md): verification, release-build checks, manual test checklist and known limitations
- [CLAUDE.md](CLAUDE.md): detailed architecture reference
- [LICENSE](LICENSE) · [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
