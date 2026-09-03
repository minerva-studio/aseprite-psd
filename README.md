# aseprite-psd

[简体中文](README.zh-CN.md)

`aseprite-psd` converts Photoshop PSD/PSB documents to and from Aseprite documents.
It is available as a native command-line program and as an Aseprite extension
that bundles the converter for import and export workflows.

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
expected to require Aseprite 1.3.18.4; follow [Aseprite #6007](https://github.com/aseprite/aseprite/issues/6007)
for the upstream status. Until that version is available, use **File > Import >
Import PSD/PSB...** and **File > Export > Export PSD/PSB...** instead.

The explicit Import command opens a modified document that is not associated
with the temporary conversion file. Once the native integration is available,
`File > Open` returns a document associated with the original PSD. For an
explicit import, press Ctrl+S or use Save As to choose the final `.aseprite`
path; Aseprite suggests the PSD's directory and base name.

The explicit Import command—and native `File > Open` once available—shows the
same import options. Choose `Automatic association` or `Preserve layers`. In
Automatic association mode, `Use metadata` selects the exact metadata preset;
when it is off, the dialog exposes the experimental association controls and
uses the normal heuristics. Legacy v1 and unmarked files use the automatic
association fallback, while damaged converter metadata opens a recovery choice
instead of being silently ignored. In particular, an unmarked PSD is
intentionally not treated as `Preserve layers`: it falls back to the standard
Automatic association path. Turn off `Use metadata` when you need to tune the
association strategy for such a file.

Exports include an invisible, versioned PSD metadata block by
default. It records only the metadata version, logical layer IDs, and
materialized cel relationships; it does not contain file paths, usernames,
device information, or usage tracking. Photoshop and other readers may ignore
this block. Use **File > Export > Aseprite ↔ Photoshop Settings...** to control both
export embedding and import usage. Disabling import usage keeps Automatic
association on the heuristic path even when metadata is present. Disabling
export embedding leaves the PSD readable, but future opens cannot use exact
converter-owned layer association from that file.

Once native integration is available, cancelling its `File > Open` import
dialog reports `PSD opening cancelled by user.` so that a cancelled open is
never confused with a failed or partially initialized document.

To export now, choose **File > Export > Export PSD/PSB...**. Native `File > Save
As...` with a `.psd` or `.psb` destination becomes an additional entry point
once the Aseprite integration is available. The extension snapshots isolated
original and flattened copies, runs the bundled converter, validates the
Photoshop document, and only then writes it through Aseprite's custom-format
save stream. The save options let you choose whether
the current frame is written as Photoshop's active frame and whether empty
pixel layers are included. Ctrl+S reuses the selected format and options.
Export always records the currently selected frame
as Photoshop's active frame. Channel compression can be selected as `ZIP`,
`ZIP prediction`, `RLE`, or `Raw`. Repeated Ctrl+S on the same sprite and
destination reuses the last successfully saved compression choice; changing
the destination or reloading the extension asks again. The explicit Export
menu command always asks independently.

Aseprite may open and truncate a native custom-format destination before the
save callback runs. The extension validates the complete PSD before writing,
but cannot provide transactional rollback for a failed overwrite. Use the
explicit Export command to write a separate destination when the existing file
must be preserved.

## Command line

Build the native CLI with Rust 1.88 or newer:

```text
cargo build --release --locked -p aseprite-psd
```

The export command accepts `--compression raw|rle|zip|zip-prediction` and
`--empty-layers include|omit`. Compression defaults to the existing
ZIP-without-prediction mode, while empty layers default to `omit`. `omit`
removes only pixel layers with no cel in any frame; a layer that is empty in
some frames still gets a hidden placeholder so frame topology stays aligned.

See [the development workflow](docs/development.md) for testing, extension
packaging, CI, and release instructions.

Inspect a PSD without writing output:

```text
aseprite-psd inspect INPUT.psd
```

Convert a PSD, refusing to replace an existing output unless `--overwrite` is
specified:

```text
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --overwrite
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association roundtrip
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical --jitter-mode repair --jitter-kind all
```

Export an Aseprite snapshot using a separately flattened snapshot produced by
Aseprite. The output extension selects PSD or PSB, and existing output is
preserved unless `--overwrite` is explicit:

```text
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite
aseprite-psd export INPUT.aseprite -o OUTPUT.psb --composite COMPOSITE.aseprite --report REPORT.json --overwrite
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --roundtrip-metadata off
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --empty-layers omit
```

Run `aseprite-psd --help` for the complete command syntax.

Frame interpretation is explicit for PSDs without a Photoshop timeline:

- `--frame-source auto` is the default. It uses a real Photoshop timeline when
  present and otherwise keeps the PSD static.
- `--frame-source static` always imports one static frame.
- `--frame-source top-level` treats each top-level layer or group as a frame.
  A top-level layer named `Background` is reported and shared by every frame.
  This mode is intended for explicitly confirmed layer-per-frame exports such
  as Procreate Animation Assist PSDs; the Procreate marker alone never enables it.

## Layer association

- `--layer-association preserve` is the default and preserves source-layer
  identity.
- `--layer-association roundtrip` restores valid v2 frame-group metadata exactly,
  uses automatic association for legacy v1 markers, preserves unmarked files,
  and exits with recovery-required status for damaged converter metadata. It
  intentionally rejects auto-only tuning flags.
- `--layer-association auto` defaults to the conservative planner, which
  prioritizes editable logical identities.
- `--association-strategy compact` explicitly prioritizes the fewest tracks
  that preserve the rendered result.
- `--association-strategy conservative` enables multilingual copy-family,
  multi-track, and candidate-folder analysis. Ambiguous identities remain
  separate.

Automatic association does not require perfect layer names. Default Photoshop
names, lazy naming, and names that drift between frames can still be resolved
when cross-frame structure, mutual exclusion, pixels, positions, ordering, and
names provide enough combined evidence. In those cases the solver restores a
stable `layer × frame` logical track without requiring manual PSD renaming. If
the evidence is insufficient, it keeps identities separate and reports the
uncertainty instead of silently merging tracks.

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

- Import packages have been validated with Aseprite 1.3.18.3 on Windows x64 and
  Ubuntu/WSL2 Linux x64 with glibc. Native PSD/PSB `File > Open` and `File >
  Save As...` integration is expected in Aseprite 1.3.18.4; track
  [Aseprite #6007](https://github.com/aseprite/aseprite/issues/6007). Earlier
  versions must use the extension's explicit Import and Export commands.
- macOS packages are built by the manual GitHub Actions workflow but have not
  yet received authentic Aseprite runtime validation.
- The extension registers PSD/PSB custom-format load and save callbacks. The
  explicit import command remains available for configurable import policies.
- Conversion preserves the normalized layer tree, RGBA8 cels, Photoshop frame
  animation, and supported layer state. Logical-layer association and some
  coordinate mappings remain experimental, so important output should be
  reviewed in Aseprite.
- PSD import preserves slice names, order, bounds, and static frame-0 keys.
  Photoshop-only group, URL, target, message, alt text, background, outsets,
  and layer-association fields are recorded as `Slices/Degraded` information
  loss. Resource 1050 versions 6/7/8 have specification-driven tests; authentic
  Photoshop samples for versions 7/8 remain unverified.
- Export preserves supported groups, static layer properties, frame duration,
  cel visibility/position/opacity, identical cel reuse, and deterministic tag
  playback. Tilemaps use the independently flattened composite snapshot and
  are reported as rasterized; tag names/boundaries, slices, color profiles,
  and per-cel Z-Index are reported when they cannot remain editable.
- Small, deterministic PSD fixtures used by automated tests live under
  `tests/fixtures/`; customer artwork and large/private PSD or PSB files are
  intentionally kept out of the repository.

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
