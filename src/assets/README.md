# Assets

Assets used by ReTone. The renderer imports them through Vite; the app icons and `Assets.car` are bundled by `src-tauri/tauri.conf.json`.

## What's here

- **App icons**: `icon.icns`, `icon.ico`, `icon.png` (same art as `src-tauri/icons/`), plus `retone.icon` and its compiled `Assets.car` for the macOS 26 app icon.
- **Tray icon**: `iconTemplate@3x.png` (a macOS template image; `src-tauri/src/tray.rs` loads it).
- **Alternate app icons**: `app-icons/` holds the icons offered in Settings › Appearance (`mono`, `light`, `aurora`, `ring`): a 512 px Dock image each, the tray templates for the alternates (`letterTemplate@3x.png`, `ringTemplate@3x.png`) and 72 px thumbnails. Only the Dock and menu-bar icons change at runtime; the bundle always carries Mono.
- **Logo**: `brand/logo.png`, the ReTone · রি-টোন wordmark used by the README.
- The brand art (the **Mono** icon) is a black R (Outfit Bold) with three tone dots (coral, amber, blue) on an off-white tile. The tray template is the R and dots alone. It is rendered outside this repo; copy updated files in rather than editing the PNGs.
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
