# psd2ase

[简体中文](README.zh-CN.md)

`psd2ase` converts Photoshop PSD documents into Aseprite documents. It is
available as a native command-line program and as an Aseprite extension that
bundles the converter for a menu-driven import workflow.

## Quick start: Aseprite extension

1. Open the [latest GitHub Release](https://github.com/minerva-studio/psd-to-ase/releases/latest).
2. Download the extension for your platform:
   - `psd2ase-aseprite-windows-x64.aseprite-extension` for Windows x64.
   - `psd2ase-aseprite-linux-x64.aseprite-extension` for Linux x64 with glibc.
3. Open the downloaded package to install it in Aseprite, then restart Aseprite
   if the command is not immediately visible.
4. Select **File > Import > Import PSD to Aseprite...** and choose a PSD.
5. Allow the extension to launch its bundled converter when Aseprite asks for
   external-program permission for the first time.

The imported sprite opens as a modified document that is not associated with
the temporary conversion file. Press Ctrl+S or use Save As to choose the final
`.aseprite` path; Aseprite suggests the PSD's directory and base name.

The extension defaults to `preserve`, which keeps source layers separate. Its
dialog also exposes the experimental automatic association modes described
below.

## Command line

Build the native CLI with Rust 1.88 or newer:

```text
cargo build --release --locked -p psd2ase
```

Build the Windows x64 Aseprite extension in one step (the script builds the
release converter and embeds it in the package):

```text
bash tools/package-aseprite-extension.sh --platform windows-x64
```

The package is written to `dist/psd2ase-aseprite-windows-x64.aseprite-extension`.
Pass `--binary PATH --no-build` when packaging a converter built elsewhere.

Inspect a PSD without writing output:

```text
psd2ase inspect INPUT.psd
```

Convert a PSD, refusing to replace an existing output unless `--overwrite` is
specified:

```text
psd2ase convert INPUT.psd -o OUTPUT.aseprite
psd2ase convert INPUT.psd -o OUTPUT.aseprite --overwrite
psd2ase convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical
psd2ase convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical --jitter-mode repair --jitter-kind all
```

Run `psd2ase --help` for the complete command syntax.

## Layer association

- `--layer-association preserve` is the default and preserves source-layer
  identity.
- `--layer-association auto --association-strategy compact` enables the
  compact cross-frame logical-layer planner.
- `--association-strategy conservative` enables multilingual copy-family,
  multi-track, and candidate-folder analysis. Ambiguous identities remain
  separate.
- Stable track order uses cross-frame overlap consensus by default. Use
  `--stable-order anchor` for anchor-frame ordering or `strict` to reject
  unresolved evidence.
- `--z-order auto` enables experimental per-cel Z-Index changes and requires
  automatic association. Conservative mode also accepts
  `--uncertain-layers flat` to disable candidate folders.

`--linked-cels identical` enables lossless reuse of equal RGBA pixel buffers on
the same automatically associated output layer. Positions, opacity, and
per-cel Z-Index remain frame-local. The default is `off`; only exact
size-and-byte matches are linked. It requires `--layer-association auto` because
`preserve` emits each source layer independently and has no cross-layer cel
reuse candidates.

## Import jitter repair

Jitter handling is disabled by default. `--jitter-mode report` only emits
diagnostics, `assist` supplies stabilized comparison evidence to automatic
association, and `repair` changes emitted cel pixels. Select `alpha`, `color`,
or `all` with `--jitter-kind`, and choose the `conservative` or `balanced`
threshold profile. Color repair is restricted to already-associated tracks
with matching size and origin; it selects a real representative cel rather
than synthesizing colors. Advanced overrides are available through
`--jitter-alpha-threshold`, `--jitter-max-speck-area`,
`--jitter-max-changed-ratio`, and `--jitter-max-channel-delta`.

## Current boundaries

- The extension packages have been validated with Aseprite 1.3.18.3 on Windows
  x64 and Ubuntu/WSL2 Linux x64 with glibc.
- macOS is not packaged or tested in this release.
- This is an import workflow extension. It does not register `.psd` with
  Aseprite's File > Open dialog.
- Conversion preserves the normalized layer tree, RGBA8 cels, Photoshop frame
  animation, and supported layer state. Logical-layer association and some
  coordinate mappings remain experimental, so important output should be
  reviewed in Aseprite.
- PSD and PSB fixtures are intentionally not committed to this repository.

## Development

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo run -p psd2ase -- --version
cargo run -p psd2ase -- --help
```

The parser and writer dependencies are the published `ag-psd` and
`aseprite-io` crates. Upstream repositories and license details are recorded in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md). The project is licensed
under the [MIT License](LICENSE).
