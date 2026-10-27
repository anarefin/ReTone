# Assets

Assets used by ReTone. The renderer imports them through Vite; the app icons and `Assets.car` are bundled by `src-tauri/tauri.conf.json`.

## What's here

- **App icons**: `icon.icns`, `icon.ico`, `icon.png` (same art as `src-tauri/icons/`), plus `retone.icon` and its compiled `Assets.car` for the macOS 26 app icon.
- **Tray icon**: `iconTemplate@3x.png` (a macOS template image; `src-tauri/src/tray.rs` loads it).
- **Logo**: `brand/logo.png`, the ReTone · রি-টোন wordmark used by the README.
- The brand art is a white Bengali **র** on a red disc in a Bangladesh-green tile, circled by two "retone" arrows. It is rendered outside this repo; copy updated files in rather than editing the PNGs.
- **Fonts**: `fonts/` holds the bundled Noto Sans (Latin) and Noto Sans Bengali (`noto-sans.css`, `noto-sans-bengali.css` + WOFF2 files, SIL OFL).
- **Provider icons**: `icons/providers/*.svg` — marks for the model providers shown in the ReTone model picker.

UI icons are not here: they are components in `src/components/icons/` (see `CLAUDE.md`).

## Rules

1. Always `import` an asset in renderer code; never reference a literal path such as `/assets/foo.webp` (packaged builds load from the `tauri://` protocol).
2. Use lossless WebP for UI art with hard edges or transparency (`cwebp -lossless -z 9 -alpha_q 100 in.png -o out.webp`); lossy only for photographic art.
3. Author raster art at 2× its CSS size, no larger.
4. Bake rounded corners into the image instead of adding CSS rounding.
5. Decorative images get `alt=""` and `aria-hidden="true"`.
6. Always set `width`/`height`, `decoding="async"` and `draggable={false}`.
7. Files under 4 KB are inlined by Vite as data URIs and won't appear in `dist/assets/`.
