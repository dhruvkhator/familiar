# Third-party notices — familiar-native

`familiar-native` and its design-system crate `crates/familiar-ui` include code and assets from the projects
below. Rust crate dependencies (GPUI via zeronsh/zui, gpui-base, resvg, …) carry their own licences in their
sources; this file covers material copied into this directory.

## zeron — MIT

<https://github.com/zeronsh/zeron> (v0.2.101). Vendored, with modifications noted in each file's header:

- `crates/familiar-ui/src/motion.rs`, `motion/windows_pulse.rs` (from `crates/ui/src/motion.rs`,
  `crates/ui/src/motion/windows_pulse.rs`, with the pure loader math from `crates/proto/src/motion.rs`)
- `crates/familiar-ui/src/edge_fade.rs` (from `crates/ui/src/edge_fade.rs`)
- `crates/familiar-ui/src/typography.rs` (from `crates/ui/src/typography.rs`)
- `crates/familiar-ui/src/appearance.rs` (from `crates/ui/src/appearance.rs`)
- `crates/familiar-ui/src/icons.rs` (from `crates/ui/src/icons.rs`)
- `crates/familiar-ui/src/notice.rs` (from `crates/ui/src/notice.rs`)
- `crates/familiar-ui/src/theme.rs` — structure and helpers (`Appearance`, appearance mirror, style generation,
  WCAG contrast helpers, pairing test) adapted from `crates/ui/src/theme.rs`; the palette is Familiar's own.
- `build.rs` follows `apps/zeron/build.rs`.

The full licence text is in `crates/familiar-ui/LICENSE-zeron`:

```
MIT License

Copyright (c) 2026 Wing

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Solar Icons — CC BY 4.0

`crates/familiar-ui/assets/icons/*.svg` (copied from zeron's `crates/ui/assets/icons/`). Most glyphs are
**Solar Icons (Linear) by 480 Design**, licensed under Creative Commons Attribution 4.0 International
(<https://creativecommons.org/licenses/by/4.0/>). `bell`, `home`, `info-circle`, `plus` and `close` are
zeron's hand-drawn glyphs in the Solar Linear style (MIT, above). No third-party brand marks are included.

## Geist and Geist Mono — SIL Open Font License 1.1

`crates/familiar-ui/assets/fonts/*.ttf` (copied from zeron's `crates/ui/assets/fonts/`, sourced from
`vercel/geist-font` v1.7.2), copyright The Geist Project Authors. The full licence is in
`crates/familiar-ui/assets/fonts/licenses/Geist-OFL.txt`. The fonts are embedded unmodified.

## App icon

`dist/windows/familiar.ico` is Familiar's own icon (copied from `apps/desktop/src-tauri/icons/icon.ico`).
