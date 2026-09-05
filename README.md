# aseprite-psd

[简体中文](README.zh-CN.md)

`aseprite-psd` converts Photoshop PSD/PSB documents to and from Aseprite documents.
It is available as a native command-line program and as an Aseprite extension
that bundles the converter for import and export workflows.

## Why aseprite-psd?

| Feature | [Tin-01](https://github.com/Tin-01/aseprite-psd-scripts) | [Resprite](https://resprite.fengeon.com/docs/files/psd) | aseprite-psd |
| --- | --- | --- | --- |
| How it runs | Aseprite Lua scripts | Built into Resprite | Aseprite extension + standalone CLI |
| Input formats | RGB/RGBA 8-bpc PackBits PSD subset | Layered PSD | PSD, PSB, Raw/RLE/ZIP |
| Photoshop Frame Animation | Single-frame import path | Not described in the documentation | Reconstructed as Aseprite frames |
| Automatic layer association | No | Not described in the documentation | Logical tracks, candidate folders, and diagnostics |
| Linked cels | No | Not described in the documentation | Identical pixels can be restored as linked cels |
| 16/32 bits per channel | Not supported | Not described in the documentation | Imported with an explicit downgrade to RGBA8 |
| PSD slices | No | Not described in the documentation | Preserves names, order, bounds, and static keys |
| Information-loss reporting | Debug log | Not described in the documentation | Versioned structured report |
| Output validation | Not described in the documentation | Not described in the documentation | Aseprite reread and structural validation |

We also look forward to [native PSD support in Aseprite](https://github.com/aseprite/aseprite/issues/114).

## Quick start: Aseprite extension

1. Open the [latest GitHub Release](https://github.com/minerva-studio/aseprite-psd/releases/latest).
2. Download `aseprite-psd-universal.aseprite-extension` for the simplest
   installation, or choose a smaller platform-specific package:
   - `aseprite-psd-windows-x64.aseprite-extension` for Windows x64.
   - `aseprite-psd-linux-x64.aseprite-extension` for Linux x64 with glibc.
   - `aseprite-psd-macos-arm64.aseprite-extension` for Apple Silicon macOS.
   - `aseprite-psd-macos-x64.aseprite-extension` for Intel macOS.
3. Open the downloaded package to install it in Aseprite, then restart Aseprite
   if the command is not immediately visible.
4. Select **File > Import > Import PSD/PSB...** and choose a Photoshop document.
5. Allow the extension to launch its bundled converter when Aseprite asks for
   external-program permission for the first time.

The macOS packages are not code-signed or notarized yet, so Gatekeeper may
restrict them after download.

Native PSD/PSB integration through `File > Open` and `File > Save As...` is
expected to require Aseprite 1.3.18.4. Until that version is available, use
**File > Import > Import PSD/PSB...** and **File > Export > Export PSD/PSB...**
instead.

## Documentation

- [User manual](docs/user-guide.md): choose a workflow by PSD structure, review results, export, and troubleshoot.
- [Options reference](docs/options.md): UI controls, CLI usage, and advanced settings.
- [Changelog](CHANGELOG.md) and [development workflow](docs/development.md).

## Development

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo run -p aseprite-psd -- --version
cargo run -p aseprite-psd -- --help
```

The parser and writer dependency is the Minerva fork of `ag-psd`, pinned to a
reviewed Git commit; `aseprite-io` remains a published crate. Upstream
repositories and license details are recorded in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md). The project is licensed
under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
