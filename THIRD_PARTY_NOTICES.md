# Third-party notices

PlumSpirit v1.0-Launch is built from its own source code plus the open-source
components listed below, and ships with one third-party data file (the GeneralUser
GS SoundFont). This file is an **inventory** of the Windows build's dependency
graph, generated from `Cargo.lock` by `tools/license_bundle.py` (which runs
`cargo tree -p plumspirit --target x86_64-pc-windows-gnullvm`); the license column is
each crate's own declaration. It is not legal advice and has not been reviewed by a
lawyer. Regenerate it whenever dependencies change.

The owner chose no license for PlumSpirit itself (see README). The v1.0-Launch
EXE is cross-built for x86_64-pc-windows-gnullvm with Rust 1.97 and llvm-mingw
20261020 (UCRT): no Microsoft Visual C++ runtime DLL is needed or bundled (the
Universal C Runtime is part of Windows 10 and 11). A native MSVC build
(`x86_64-pc-windows-msvc`, see README) is equivalent; its runtime notice is
MICROSOFT_RUNTIME_NOTICE.txt in the source tree.

## Things to know before distributing

* **Bundled SoundFont.** `SoundFonts/GeneralUser-GS.sf2` is **GeneralUser GS v2.0.3**
  by S. Christian Collins (<https://www.schristiancollins.com>,
  <https://github.com/mrbumpy409/GeneralUser-GS>, commit 684543d5). Its license
  (`SoundFonts/GeneralUser-GS-LICENSE.txt`, also reproduced in
  `THIRD_PARTY_LICENSES.txt`) allows unrestricted use for music creation and use in
  software projects, including modification and repackaging. **Caveat stated by the
  author:** some samples were taken from other freely available banks and their
  origin cannot be verified with 100% certainty; he notes this may matter for
  commercial software products. The author also asks that websites not link
  directly to his download files. Review this before any commercial distribution.
* **v0.8 built-in plugins** (Help → About PlumSpirit lists the same credits):
  * **Spiderweb** by Kanade Tachibana — <https://github.com/UnPrioritized/Spiderweb>
    (commit f7fd766), MIT. Its shape / notes engine is ported to Rust
    (`crates/plumspirit_engine/src/spiderweb/`) and expanded for PlumSpirit by
    ouko; the MIT notice (© 2026 Kanade Tachibana) is reproduced in
    `THIRD_PARTY_LICENSES.txt`. Spiderweb's bundled BASS / BASSMIDI libraries
    are not used.
  * **SAFOR — Simple And Fast Overlaps Remover** by DixelU —
    <https://github.com/DixelU/SAFOR> (commit e73cf76), Apache-2.0. Its
    algorithm is reimplemented natively (no source file copied); the Apache-2.0
    text is in `THIRD_PARTY_LICENSES.txt`.
  * **Image to MIDI** (ImageToMidi by arduano), **Audio Reference** (dominohook
    by Khang) and **HZ-Chop** (Lua script by Softy107 for Andromeda) are
    credited by name; their sources carry no licence, so nothing was copied:
    the behaviour was re-implemented from scratch (HZ-Chop's Lua is used only
    as a development test oracle and is not shipped). BASS is not used.
* **Added in v0.9** (V09-S47): the `image` crate's TIFF decoder for
  Displacement Map's picture maps — `tiff` 0.11 and `fax` 0.2 (MIT);
  inventory regenerated with `tools/license_bundle.py` for the shipped
  `x86_64-pc-windows-gnullvm` target: 162 compiled-in and 18 build-only
  crates, every licence text found.
* **Added in v0.8** (all permissive; inventory regenerated with
  `tools/license_bundle.py` for the shipped `x86_64-pc-windows-gnullvm`
  target: 160 compiled-in and 18 build-only crates, every licence text found): `image` 0.25 with its PNG /
  JPEG / BMP / GIF / WebP decoders, `skrifa` 0.44 (glyph outlines), `claxon`
  (Apache-2.0, FLAC), `lewton` + `ogg` (MIT / Apache-2.0, BSD-3-Clause; Ogg
  Vorbis), `nanomp3` (MIT / Apache-2.0, MP3), `byteorder`, `tinyvec`, and for the
  Vulkan renderer `egui-wgpu` / `wgpu` 30 (Vulkan backend only) with `ash`,
  `gpu-allocator`, `presser`, `pollster`, `allocator-api2`, `ordered-float`
  (MIT / Apache-2.0) and `spirv` (Apache-2.0; licence text taken from
  <https://github.com/gfx-rs/rspirv>, as the crate ships none; `egui-wgpu`'s
  from the egui repository). The per-crate tables below are the release
  build's (regenerated 2026-10-05).
* **Added in v1.0-Launch (L-S45):** the memory allocator of the Windows build —
  `mimalloc` 0.1.52 and `libmimalloc-sys` 0.1.49 (MIT) with **mimalloc 2.3.2** by Microsoft
  Corporation / Daan Leijen (MIT) compiled into PlumSpirit.exe from the crate's C sources
  (its copyright and permission notice is in `THIRD_PARTY_LICENSES.txt`). Measured on the
  owner's PC: closing a 224M-note project 3.0 s → 0.5 s without the 130 ms stall, the
  analysis index 8.4 s → 1.5 s, import 1.7 s → 1.4 s. Linux builds keep the system
  allocator. Inventory regenerated with `tools/license_bundle.py --target
  x86_64-pc-windows-gnullvm`: 181 compiled-in and 22 build-only crates, every licence
  text found.
* **Added in v1.0-Launch (L-D01 / L-S09 / L-S10):** archive decoders, all pure Rust and
  permissive — `flate2` 1.1 on `miniz_oxide` (MIT OR Apache-2.0; zip deflate, gzip),
  `lzma-rust2` (Apache-2.0; xz, 7z LZMA / LZMA2) with `sha2` (MIT OR Apache-2.0),
  `ruzstd` (MIT) with `twox-hash` (MIT; zstd), `crc32fast` (MIT OR Apache-2.0) and
  `sevenz-rust2` 0.24 without default features (Apache-2.0; 7z). Inventory regenerated
  with `tools/license_bundle.py --target x86_64-pc-windows-gnullvm`: 179 compiled-in and
  22 build-only crates, every licence text found.
* **Added in v1.0-beta1 (B1-0020):** the Lua scripting host — `mlua` 0.12 and
  `mlua-sys` (MIT; `mlua-sys` ships no license file, its text is the mlua
  repository's) with `bstr` and `libc` (MIT OR Apache-2.0), and the **Lua 5.4.9
  interpreter** (Lua.org, PUC-Rio, MIT) compiled into PlumSpirit.exe from the
  `lua-src` crate's C sources (`lua-src`, `cc`, `pkg-config`, `which` are build
  tools only). The Lua copyright and permission notice is in
  `THIRD_PARTY_LICENSES.txt`.
* **v0.5 adds no third-party components**: its `Cargo.lock` differs from v0.4's
  only in PlumSpirit's own version numbers (regenerated with
  `tools/license_bundle.py` for this release: 115 compiled-in and 18 build-only
  crates, every license text found, the same `THIRD_PARTY_LICENSES.txt`).
* **v0.4 added no third-party components** either: its dependency graph is the same as
  v0.3's (regenerated with `tools/license_bundle.py` and compared for this release;
  v0.4 only enables one more feature of `windows-sys`, `Win32_Media_Audio`, for
  external MIDI outputs).
* **v0.3 added no third-party components** either (same graph as v0.2).
* **Added in v0.2:** `rustysynth` (MIT, SoundFont synthesis), `cpal` (Apache-2.0,
  audio output), `dasp_sample` (MIT OR Apache-2.0), and Microsoft's `windows` crates
  0.62 (`windows`, `windows-core`, `-collections`, `-future`, `-implement`,
  `-interface`, `-numerics`, `-result`, `-strings`, `-threading`; MIT OR
  Apache-2.0) used by CPAL's WASAPI backend. `rustysynth` and `dasp_sample` ship no
  license file in their published crates; their texts were taken from the projects'
  repositories (<https://github.com/sinshu/rustysynth>,
  <https://github.com/RustAudio/dasp>).
* **Fonts embedded in the executable.** `epaint_default_fonts` (egui's fallback
  fonts: Ubuntu-Light, Hack, Noto Emoji, emoji-icon-font) is compiled into
  PlumSpirit.exe. It is covered by `(MIT OR Apache-2.0) AND OFL-1.1 AND
  Ubuntu-font-1.0`; the OFL and Ubuntu Font Licence texts are in
  `THIRD_PARTY_LICENSES.txt`. These fonts are only used when neither Helvetica nor
  Arial can be loaded.
* **Helvetica is never bundled.** PlumSpirit only reads Helvetica if it is already
  installed on the user's computer.
* **Dual-licensed options.** `self_cell` is `Apache-2.0 OR GPL-2.0-only`; PlumSpirit
  uses it under Apache-2.0. Crates offering `MIT OR Apache-2.0` (or with Zlib,
  Unlicense, 0BSD alternatives) can be used under MIT or Apache-2.0.
* **Other licenses present:** BSL-1.0 (`clipboard-win`, `error-code`), ISC
  (`libloading`), Unicode-3.0 (`unicode-ident` data), Zlib, BSD-2/3-Clause options.
  All are permissive; each requires keeping its copyright/license notice.
* **Rust standard library** (statically linked): MIT OR Apache-2.0 (its copyright
  file is reproduced in `THIRD_PARTY_LICENSES.txt`).
* **The delivered v1.0-Launch PlumSpirit.exe** is cross-built on Linux for
  x86_64-pc-windows-gnullvm (Rust 1.97, llvm-mingw 20261020 UCRT). Statically linked
  runtime parts: the Rust standard library, LLVM's libunwind and compiler-rt builtins
  (Apache-2.0 WITH LLVM-exception) and mingw-w64's CRT start-up code / libmingwex
  (ZPL-2.1 and others); their texts are in `THIRD_PARTY_LICENSES.txt`. No GCC runtime
  is linked. Imports, compiler, source commit and hash are in docs\BUILD_INFO.txt.
* **Full license texts** for everything listed here are collected in
  `THIRD_PARTY_LICENSES.txt`, shipped next to PlumSpirit.exe.
* **Branding**: the PlumSpirit logo and wordmark (`assets/brand/`, `assets/icon/`)
  are the owner's own artwork, supplied in the v1.0-beta1 proposal; no third-party
  font or image is bundled (`docs/references/v1_0_beta1/LOGO_PROVENANCE.md`).
* Linux development builds additionally use X11/Wayland crates and, for audio,
  `alsa` 0.11 (Apache-2.0/MIT) and `alsa-sys` 0.4 (MIT). They are not part of the
  Windows executable and are not listed below.

## Compiled into PlumSpirit.exe (181 crates)

| Crate | Version | License (as declared) | Source |
|---|---|---|---|
| accesskit | 0.24.1 | MIT OR Apache-2.0 | https://github.com/AccessKit/accesskit |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | https://github.com/oyvindln/adler2 |
| ahash | 0.8.12 | MIT OR Apache-2.0 | https://github.com/tkaitchuck/ahash |
| allocator-api2 | 0.2.21 | MIT OR Apache-2.0 | https://github.com/zakarumych/allocator-api2 |
| arboard | 3.6.1 | MIT OR Apache-2.0 | https://github.com/1Password/arboard |
| arrayvec | 0.7.8 | MIT OR Apache-2.0 | https://github.com/bluss/arrayvec |
| ash | 0.38.0+1.3.281 | MIT OR Apache-2.0 | https://github.com/ash-rs/ash |
| bit-set | 0.10.0 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-set |
| bit-vec | 0.9.1 | Apache-2.0 OR MIT | https://github.com/contain-rs/bit-vec |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | https://github.com/bitflags/bitflags |
| block-buffer | 0.12.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| bstr | 1.13.1 | MIT OR Apache-2.0 | https://github.com/BurntSushi/bstr |
| bytemuck | 1.25.2 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/bytemuck |
| bytemuck_derive | 1.12.1 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/bytemuck |
| byteorder | 1.5.0 | Unlicense OR MIT | https://github.com/BurntSushi/byteorder |
| byteorder-lite | 0.1.0 | Unlicense OR MIT | https://github.com/image-rs/byteorder-lite |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/cfg-if |
| claxon | 0.4.3 | Apache-2.0 | https://github.com/ruuda/claxon |
| clipboard-win | 5.4.1 | BSL-1.0 | https://github.com/DoumanAsh/clipboard-win |
| codespan-reporting | 0.13.1 | Apache-2.0 | https://github.com/brendanzab/codespan |
| color | 0.3.3 | Apache-2.0 OR MIT | https://github.com/linebender/color |
| color_quant | 1.1.0 | MIT | https://github.com/image-rs/color_quant.git |
| const-oid | 0.10.2 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| cpal | 0.18.2 | Apache-2.0 | https://github.com/RustAudio/cpal |
| cpufeatures | 0.3.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| crc32fast | 1.5.2 | MIT OR Apache-2.0 | https://github.com/srijs/rust-crc32fast |
| crypto-common | 0.2.2 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| cursor-icon | 1.2.0 | MIT OR Apache-2.0 OR Zlib | https://github.com/rust-windowing/cursor-icon |
| dasp_sample | 0.11.0 | MIT OR Apache-2.0 | https://github.com/rustaudio/sample.git |
| digest | 0.11.3 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| document-features | 0.2.12 | MIT OR Apache-2.0 | https://github.com/slint-ui/document-features |
| dpi | 0.1.2 | Apache-2.0 AND MIT | https://github.com/rust-windowing/winit |
| ecolor | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui |
| eframe | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui/tree/main/crates/eframe |
| egui | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui |
| egui-wgpu | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui/tree/main/crates/egui-wgpu |
| egui-winit | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui/tree/main/crates/egui-winit |
| egui_glow | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui/tree/main/crates/egui_glow |
| either | 1.18.0 | MIT OR Apache-2.0 | https://github.com/rayon-rs/either |
| emath | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui/tree/main/crates/emath |
| epaint | 0.36.2 | MIT OR Apache-2.0 | https://github.com/emilk/egui/tree/main/crates/epaint |
| epaint_default_fonts | 0.36.2 | (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 | https://github.com/emilk/egui/tree/main/crates/epaint_default_fonts |
| equivalent | 1.0.2 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/equivalent |
| error-code | 3.4.0 | BSL-1.0 | https://github.com/DoumanAsh/error-code |
| euclid | 0.22.14 | MIT OR Apache-2.0 | https://github.com/servo/euclid |
| fax | 0.2.7 | MIT | https://github.com/pdf-rs/fax |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 | https://github.com/image-rs/fdeflate |
| fearless_simd | 0.4.1 | Apache-2.0 OR MIT | https://github.com/linebender/fearless_simd |
| flate2 | 1.1.10 | MIT OR Apache-2.0 | https://github.com/rust-lang/flate2-rs |
| foldhash | 0.2.0 | Zlib | https://github.com/orlp/foldhash |
| font-types | 0.12.5 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| gif | 0.14.2 | MIT OR Apache-2.0 | https://github.com/image-rs/image-gif |
| glow | 0.17.0 | MIT OR Apache-2.0 OR Zlib | https://github.com/grovesNL/glow |
| glutin | 0.32.3 | Apache-2.0 | https://github.com/rust-windowing/glutin |
| glutin-winit | 0.5.0 | MIT | https://github.com/rust-windowing/glutin |
| glutin_egl_sys | 0.7.1 | Apache-2.0 | https://github.com/rust-windowing/glutin |
| glutin_wgl_sys | 0.6.1 | Apache-2.0 | https://github.com/rust-windowing/glutin |
| gpu-allocator | 0.28.0 | MIT OR Apache-2.0 | https://github.com/Traverse-Research/gpu-allocator |
| guillotiere | 0.7.0 | MIT/Apache-2.0 | https://github.com/nical/guillotiere |
| half | 2.7.1 | MIT OR Apache-2.0 | https://github.com/VoidStarKat/half-rs |
| harfrust | 0.12.0 | MIT | https://github.com/harfbuzz/harfrust |
| hashbrown | 0.16.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hybrid-array | 0.4.15 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hybrid-array |
| image | 0.25.10 | MIT OR Apache-2.0 | https://github.com/image-rs/image |
| image-webp | 0.2.4 | MIT OR Apache-2.0 | https://github.com/image-rs/image-webp |
| indexmap | 2.14.2 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/indexmap |
| itertools | 0.15.0 | MIT OR Apache-2.0 | https://github.com/rust-itertools/itertools |
| itoa | 1.0.18 | MIT OR Apache-2.0 | https://github.com/dtolnay/itoa |
| kurbo | 0.13.1 | Apache-2.0 OR MIT | https://github.com/linebender/kurbo |
| lewton | 0.10.2 | MIT OR Apache-2.0 | https://github.com/RustAudio/lewton |
| libc | 0.2.189 | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| libloading | 0.8.9 | ISC | https://github.com/nagisa/rust_libloading/ |
| libm | 0.2.16 | MIT | https://github.com/rust-lang/compiler-builtins |
| libmimalloc-sys | 0.1.49 | MIT | https://github.com/purpleprotocol/mimalloc_rust/tree/master/libmimalloc-sys |
| linebender_resource_handle | 0.1.1 | Apache-2.0 OR MIT | https://github.com/linebender/raw_resource_handle |
| litrs | 1.0.0 | MIT OR Apache-2.0 | https://github.com/LukasKalbertodt/litrs |
| lock_api | 0.4.14 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| log | 0.4.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/log |
| lzma-rust2 | 0.23.0 | Apache-2.0 | https://github.com/hasenbanck/lzma-rust2/ |
| lzma-rust2 | 0.24.0 | Apache-2.0 | https://github.com/hasenbanck/lzma-rust2/ |
| memchr | 2.8.3 | Unlicense OR MIT | https://github.com/BurntSushi/memchr |
| mimalloc | 0.1.52 | MIT | https://github.com/purpleprotocol/mimalloc_rust |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| miniz_oxide | 0.9.1 | MIT OR Zlib OR Apache-2.0 | https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide |
| mlua | 0.12.2 | MIT | https://github.com/mlua-rs/mlua |
| mlua-sys | 0.13.0 | MIT | https://github.com/mlua-rs/mlua |
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 | https://github.com/awxkee/moxcms.git |
| naga | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| naga-types | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| nanomp3 | 0.2.0 | MIT OR Apache-2.0 | https://github.com/robbie01/nanomp3 |
| nanomp3-core | 0.2.0 | MIT OR Apache-2.0 | https://github.com/robbie01/nanomp3 |
| nohash-hasher | 0.2.0 | Apache-2.0 OR MIT | https://github.com/paritytech/nohash-hasher |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | https://github.com/rust-num/num-traits |
| ogg | 0.8.0 | BSD-3-Clause | https://github.com/RustAudio/ogg |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | https://github.com/matklad/once_cell |
| ordered-float | 5.5.0 | MIT | https://github.com/reem/rust-ordered-float |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| parking_lot_core | 0.9.12 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot |
| peniko | 0.6.1 | Apache-2.0 OR MIT | https://github.com/linebender/peniko |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT | https://github.com/taiki-e/pin-project-lite |
| png | 0.18.1 | MIT OR Apache-2.0 | https://github.com/image-rs/image-png |
| pollster | 1.0.1 | Apache-2.0/MIT | https://github.com/zesterer/pollster |
| polycool | 0.4.0 | MIT OR Apache-2.0 | https://github.com/linebender/kurbo |
| presser | 0.3.1 | MIT OR Apache-2.0 | https://github.com/EmbarkStudios/presser |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| profiling | 1.0.18 | MIT OR Apache-2.0 | https://github.com/aclysma/profiling |
| pxfm | 0.1.30 | BSD-3-Clause OR Apache-2.0 | https://github.com/awxkee/pxfm |
| quick-error | 2.0.1 | MIT/Apache-2.0 | http://github.com/tailhook/quick-error |
| quote | 1.0.47 | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| raw-window-handle | 0.6.2 | MIT OR Apache-2.0 OR Zlib | https://github.com/rust-windowing/raw-window-handle |
| read-fonts | 0.41.0 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| renderdoc-sys | 1.1.0 | MIT OR Apache-2.0 | https://github.com/ebkalderon/renderdoc-rs |
| rfd | 0.17.2 | MIT | https://github.com/PolyMeilex/rfd |
| rustc-hash | 1.1.0 | Apache-2.0/MIT | https://github.com/rust-lang-nursery/rustc-hash |
| rustc-hash | 2.1.3 | Apache-2.0 OR MIT | https://github.com/rust-lang/rustc-hash |
| rustysynth | 1.3.6 | MIT | https://github.com/sinshu/rustysynth |
| ruzstd | 0.9.1 | MIT | https://github.com/KillingSpark/zstd-rs |
| scopeguard | 1.2.0 | MIT OR Apache-2.0 | https://github.com/bluss/scopeguard |
| self_cell | 1.3.0 | Apache-2.0 OR GPL-2.0-only | https://github.com/Voultapher/self_cell |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_json | 1.0.151 | MIT OR Apache-2.0 | https://github.com/serde-rs/json |
| sevenz-rust2 | 0.24.1 | Apache-2.0 | https://github.com/hasenbanck/sevenz-rust |
| sha2 | 0.11.1 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| simd-adler32 | 0.3.10 | MIT | https://github.com/mcountryman/simd-adler32 |
| skrifa | 0.44.0 | MIT OR Apache-2.0 | https://github.com/googlefonts/fontations |
| smallvec | 1.16.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-smallvec |
| smol_str | 0.2.2 | MIT OR Apache-2.0 | https://github.com/rust-analyzer/smol_str |
| spirv | 0.4.0+sdk-1.4.341.0 | Apache-2.0 | https://github.com/gfx-rs/rspirv |
| static_assertions | 1.1.0 | MIT OR Apache-2.0 | https://github.com/nvzqz/static-assertions-rs |
| syn | 2.0.119 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| syn | 3.0.6 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| thiserror-impl | 2.0.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror |
| tiff | 0.11.3 | MIT | https://github.com/image-rs/image-tiff |
| tinyvec | 1.11.0 | Zlib OR Apache-2.0 OR MIT | https://github.com/Lokathor/tinyvec |
| tinyvec_macros | 0.1.1 | MIT OR Apache-2.0 OR Zlib | https://github.com/Soveu/tinyvec_macros |
| tracing | 0.1.44 | MIT | https://github.com/tokio-rs/tracing |
| tracing-core | 0.1.36 | MIT | https://github.com/tokio-rs/tracing |
| twox-hash | 2.1.5 | MIT | https://github.com/shepmaster/twox-hash |
| type-map | 0.5.1 | MIT/Apache-2.0 | https://github.com/kardeiz/type-map |
| typenum | 1.20.1 | MIT OR Apache-2.0 | https://github.com/paholg/typenum |
| unicode-bidi | 0.3.18 | MIT OR Apache-2.0 | https://github.com/servo/unicode-bidi |
| unicode-general-category | 1.1.0 | Apache-2.0 | https://github.com/yeslogic/unicode-general-category |
| unicode-ident | 1.0.26 | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| unicode-linebreak | 0.1.5 | Apache-2.0 | https://github.com/axelf4/unicode-linebreak |
| unicode-script | 0.5.8 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-script |
| unicode-segmentation | 1.13.3 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-segmentation |
| unicode-width | 0.2.2 | MIT OR Apache-2.0 | https://github.com/unicode-rs/unicode-width |
| uuid | 1.26.1 | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| vello_common | 0.1.0 | Apache-2.0 OR MIT | https://github.com/linebender/vello |
| vello_cpu | 0.1.0 | Apache-2.0 OR MIT | https://github.com/linebender/vello |
| web-time | 1.1.0 | MIT OR Apache-2.0 | https://github.com/daxpedda/web-time |
| weezl | 0.1.12 | MIT OR Apache-2.0 | https://github.com/image-rs/weezl |
| wgpu | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-core | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-core-deps-windows-linux-android | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-hal | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-naga-bridge | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| wgpu-types | 30.0.1 | MIT OR Apache-2.0 | https://github.com/gfx-rs/wgpu |
| windows | 0.62.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-collections | 0.3.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-core | 0.62.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-future | 0.3.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-implement | 0.60.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-interface | 0.59.3 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-numerics | 0.3.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-result | 0.4.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-strings | 0.5.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.52.0 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.60.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-targets | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-targets | 0.53.5 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows-threading | 0.2.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnullvm | 0.52.6 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| windows_x86_64_gnullvm | 0.53.1 | MIT OR Apache-2.0 | https://github.com/microsoft/windows-rs |
| winit | 0.30.13 | Apache-2.0 | https://github.com/rust-windowing/winit |
| zerocopy | 0.8.59 | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zerocopy-derive | 0.8.59 | BSD-2-Clause OR Apache-2.0 OR MIT | https://github.com/google/zerocopy |
| zmij | 1.0.23 | MIT | https://github.com/dtolnay/zmij |
| zune-core | 0.5.3 | MIT OR Apache-2.0 OR Zlib | https://github.com/etemesi254/zune-image |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib | https://github.com/etemesi254/zune-image/tree/dev/crates/zune-jpeg |

## Build tools only (22 crates, not shipped)

| Crate | Version | License (as declared) | Source |
|---|---|---|---|
| autocfg | 1.5.1 | Apache-2.0 OR MIT | https://github.com/cuviper/autocfg |
| cc | 1.5.1 | MIT OR Apache-2.0 | https://github.com/rust-lang/cc-rs |
| cfg_aliases | 0.2.2 | MIT | https://github.com/katharostech/cfg_aliases |
| embed-resource | 3.0.11 | MIT | https://github.com/nabijaczleweli/rust-embed-resource |
| find-msvc-tools | 0.1.14 | MIT OR Apache-2.0 | https://github.com/rust-lang/cc-rs |
| gl_generator | 0.14.0 | Apache-2.0 | https://github.com/brendanzab/gl-rs/ |
| khronos_api | 3.1.0 | Apache-2.0 | https://github.com/brendanzab/gl-rs/ |
| lua-src | 551.0.2 | MIT | https://github.com/mlua-rs/lua-src-rs |
| luajit-src | 210.7.4+c6ffc14 | MIT | https://github.com/mlua-rs/luajit-src-rs |
| pkg-config | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/pkg-config-rs |
| rustc_version | 0.4.1 | MIT OR Apache-2.0 | https://github.com/djc/rustc-version-rs |
| semver | 1.0.28 | MIT OR Apache-2.0 | https://github.com/dtolnay/semver |
| serde_spanned | 1.1.1 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| shlex | 2.0.1 | MIT OR Apache-2.0 | https://github.com/comex/rust-shlex |
| toml | 1.1.6+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 1.1.1+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_parser | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_writer | 1.1.2+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| version_check | 0.9.5 | MIT/Apache-2.0 | https://github.com/SergioBenitez/version_check |
| which | 8.0.6 | MIT | https://github.com/harryfei/which-rs.git |
| winnow | 1.0.4 | MIT | https://github.com/winnow-rs/winnow |
| xml-rs | 0.8.29 | MIT | https://github.com/kornelski/xml-rs |

Harfrust0.12.0 uses the exact previously locked MIT source in
`third_party/harfrust-0.12.0`, with one additive `GlyphBuffer::successful()` accessor
to reject native operation/buffer exhaustion. Its shaping algorithms are unchanged.
See `PLUMSPIRIT_PATCH.md`, `PLUMSPIRIT_PROVENANCE.json` and the upstream LICENSE there.
External-font cache/runtime isolation is implemented by PlumSpirit, not by a library upgrade.
