# Third-party notices

LeanPDF itself is MIT-licensed (see `LICENSE`). It uses the following software, each under its own license.

## Shipped with the app

| Component | Used for | License |
| --- | --- | --- |
| [PDFium](https://pdfium.googlesource.com/pdfium/) (prebuilt `pdfium.dll` from [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries), Chromium build 7881) | Rendering, text extraction, annotations, saving | BSD-3-Clause / Apache-2.0. The license texts for PDFium and everything bundled inside it (FreeType, ICU, lcms, OpenJPEG, libjpeg-turbo, libpng, libtiff, zlib, Abseil and others) are in `pdfium/licenses/` and are installed with the app in `licenses/`. |

## Compiled into the app

| Component | License |
| --- | --- |
| [pdfium-render](https://github.com/ajrcarey/pdfium-render) 0.9.4 (vendored in `vendor/pdfium-render` with one added function, `save_incremental_to_writer`) | MIT OR Apache-2.0 |
| [egui / eframe](https://github.com/emilk/egui) (native app) | MIT OR Apache-2.0 |
| [Tauri](https://tauri.app/) and its plugins (web-view app) | MIT OR Apache-2.0 |
| App icon: the egui/eframe default window icon | MIT OR Apache-2.0 |
| [rusqlite](https://github.com/rusqlite/rusqlite) with bundled [SQLite](https://sqlite.org/) | MIT; SQLite is public domain |
| [ureq](https://github.com/algesten/ureq) and native-tls (Windows Schannel) for the AI requests | MIT OR Apache-2.0 |
| [keyring](https://github.com/open-source-cooperative/keyring-rs) for storing API keys in Windows Credential Manager | MIT OR Apache-2.0 |
| [rfd](https://github.com/PolyMeilex/rfd) | MIT |
| walkdir, png, chrono, serde, serde_json, windows-sys, winresource | MIT OR Apache-2.0 |

The web-view app additionally relies on Microsoft's WebView2 runtime, which Windows 10/11 provides.

The complete list of Rust dependencies and their licenses is in each crate's `Cargo.lock`; `cargo tree` or `cargo-license` will print it.
