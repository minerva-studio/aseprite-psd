# Development workflow

## License policy

Project-authored code is available under the [MIT License](../LICENSE-MIT) or
the [Apache License, Version 2.0](../LICENSE-APACHE), at the user's option.
New project-authored code follows the same dual-license policy. Third-party
code and dependencies retain their own licenses, which are recorded in
`THIRD_PARTY_LICENSES.md` when applicable.

## Phase gates

1. **Toolchain:** workspace, dependency versions, license records, and CI build
   matrix are healthy.
2. **PSD compatibility probe:** an unmodified parser is compared with the
   TypeScript oracle on a representative PSD before any writer work is enabled.
3. **Normalized model:** layer ownership, frame state, and pixel lifetime are
   explicit and format-independent.
4. **Format writers:** output is written transactionally, read back, and
   structurally and visually validated before replacement.
5. **CLI and release:** exit codes, reports, platform artifacts, and manual
   Aseprite checks are complete.

## Local commands

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo run -p aseprite-psd -- --version
cargo run -p aseprite-psd -- --help
npm --prefix tools/ag-psd-oracle install --ignore-scripts --allow-git=all --cache tools/ag-psd-oracle/.npm-cache
pwsh -File tools/probe.ps1 -InputPath 'path\to\fixture.psd'
pwsh -File tools/render-aseprite-frames.ps1 \
  -InputPath target/probe/preserve.aseprite \
  -OutputDirectory target/probe/preserve-frames -FrameCount 12
# Compare frame-indexed Aseprite renders; visible/alpha differences fail.
node tools/compare-aseprite-renders.mjs \
  --left target/probe/preserve-frames \
  --right target/probe/auto-frames \
  --output target/probe/render-diff.json
# Run the Aseprite-hosted Lua module smoke check (Windows example; Aseprite on PATH)
$Aseprite = (Get-Command aseprite -ErrorAction Stop).Source
& $Aseprite -b `
  --script-param extensionRoot=$PWD/extensions/aseprite-psd `
  --script extensions/aseprite-psd/tests/smoke.lua
```

Use a real PSD supplied outside the repository for compatibility testing. Do
Small deterministic fixtures used by automated tests belong under
`tests/fixtures/`. Do not add customer artwork or private fixtures to Git;
keep generated probes and manual review material under the ignored `.probe/`,
`target/`, or `dist/` directories.

Use `-OutputDirectory` when probing multiple fixtures so their snapshots do not
share files. The render comparator consumes frame-indexed `frame-N.png` files
from two directories and writes per-frame and aggregate visible, Alpha, and
transparent-RGB-only differences. Only visible or Alpha differences fail. When
stable source order emits a documented Z-order diagnostic, repeat the visual
gate with `--z-order auto`; stable and auto are separate conversion contracts.

## Packaging and release

The extension packaging scripts build the native release converter by default
and write packages under `dist/`. On Linux or macOS, use Bash with the platform
that matches the current machine:

```text
bash tools/package-aseprite-extension.sh --platform linux-x64
bash tools/package-aseprite-extension.sh --platform macos-arm64
bash tools/package-aseprite-extension.sh --platform macos-x64
```

On Windows, use the native PowerShell entry point:

```powershell
.\tools\package-aseprite-extension.ps1 -Platform windows-x64
```

Pass `--binary PATH --no-build`, or `-Binary PATH -NoBuild` in PowerShell, to
package a converter that was built separately. Linux and macOS require the
`zip` and `unzip` commands; Windows uses `Compress-Archive`.

The Universal package is a multi-platform extension, not one fat executable.
Prepare the four native converters with this layout:

```text
universal-input/windows-x64/aseprite-psd.exe
universal-input/linux-x64/aseprite-psd
universal-input/macos-arm64/aseprite-psd
universal-input/macos-x64/aseprite-psd
```

Then assemble it on Linux or macOS so Unix executable permissions are
preserved:

```text
bash tools/package-aseprite-extension.sh --platform universal \
  --binary-dir universal-input --no-build
```

The packaging workflow first builds and verifies the CLI on four native
runners, creates the four platform-specific extension packages, and then
assembles `aseprite-psd-universal.aseprite-extension` from those converters.
A manual `workflow_dispatch` run uploads the five packages as artifacts without
publishing a release.

Pushing a `v*` tag runs the same pipeline and creates or updates the matching
GitHub Release. Create and push the tag explicitly:

```text
git tag v0.3.0
git push origin v0.3.0
```

GitHub Actions never creates a tag. Re-running the workflow for an existing tag
updates the five release assets instead of creating a duplicate release.

## Test layout

Unit tests are centralized under each crate's `src/tests/` directory and are
split by the production owner (`core`, `layer_names`, `aseprite_writer`,
`logical_layers`, and `photoshop_animation`). Production modules only declare
their test module with `#[cfg(test)]` and a `#[path = "tests/..."]` attribute;
the tests therefore retain private access without scattering test bodies beside
production code. The CLI follows the same convention under
`crates/aseprite-psd-cli/src/tests/`. Crate-level `tests/` directories are reserved
for future black-box integration tests that exercise only public APIs.

The probe runner requires an input path. Set `ASEPRITE_PSD_FIXTURE` or pass
`-InputPath` to select a local fixture. Use `-OutputDirectory` when probing
multiple fixtures so their snapshots do not share files. It writes only ignored
JSON snapshots under `target/probe/`,
verifies the source file's size and SHA-256 before and after the run, and never
creates an Aseprite file.

The render comparator consumes frame-indexed `frame-N.png` files from two
directories. It writes a JSON report with per-frame and aggregate visible,
Alpha, and transparent-RGB-only differences. Only visible or Alpha differences
fail the command; transparent RGB differences are retained as diagnostic data.

Stage 3 adds a Photoshop frame-animation gate to the same probe command. The
Rust scanner reads bounded image-resource and layer additional-info sections
(4000/4003, shmd/mlst/mdyn) and converts them into the format-independent
animation model in aseprite-psd-core. The TypeScript `ag-psd` master snapshot is
pinned as the development oracle for overlapping parser behaviour; the
Minerva-maintained Rust `ag-psd` fork remains the product runtime and includes
documented extensions beyond that oracle.

Stage 4 exposes `aseprite_psd_core::normalize` as the reader boundary. It owns the
recursive layer tree, validated document bounds, copied RGBA8 pixels, and
frame-local layer state. A static PSD becomes one normalized frame with no
duration; a future Aseprite serializer may choose its own 100 ms default at
serialization time. The probe compares both the authored animation view and
the complete normalized-document view. This was the last stage before writer
activation.

Stage 5 enables the minimal experimental writer. It preserves the normalized
tree and RGBA8 cels, creates one Aseprite frame per normalized frame, and
validates the serialized file by reading it back before committing output. The
first output is `target/first-result/鹦鹉走路.aseprite`; its cel-origin policy is
explicitly provisional (`pixels.left/top` plus the frame-local PSD offset), and
all unsupported mappings are reported as warnings.

## Ownership rules

- `aseprite-psd-core` owns normalized document semantics, pixel ownership, and
  conversion invariants. PSD and Aseprite writers use this model as their only
  conversion boundary.
- `LayerAssociation` is the conversion-policy source of truth. Preserve mode
  carries no automatic-only settings; auto mode carries `AutoAssociationOptions`,
  and only the conservative strategy can carry an uncertain-layer policy.
- The logical-layer planner is staged inside `logical_layers/`: observation
  discovery and selector evidence live in `observation.rs`, association and
  scoring in `association.rs`, stable ordering and Z-Index work in
  `ordering.rs`, and candidate-folder/layout topology in `layout.rs`.
- The planner builds one `ObservationStore` per normalized document.
  Source-layer names, paths, group evidence, frame-container evidence, and
  borrowed RGBA pixels are stored once; frame observations and track history
  refer back to that evidence instead of cloning image buffers.
- `AssociationEngine` owns mutable tracks, decisions, family preassignments, and
  selector evidence for one planning run. It transfers an owned
  `AssociationOutput` to ordering and layout after matching. Compact and
  conservative association remain explicit branches; the shared weighted
  matcher lives in `logical_layers/matching.rs`, while report text is derived
  from decisions in `logical_layers/report.rs`.
- Layout owns persistent and candidate group identities through `GroupKey`.
  Candidate folders never reuse a source layer ID and are validated against the
  public planned topology before writing.
- `aseprite-psd` owns argument parsing, output policy, exit codes, and presentation.
- The Aseprite adapter keeps `aseprite-psd.lua` as a registration-only entrypoint.
  `lib/process.lua` owns the converter process and temporary-file boundary,
  `lib/dialogs.lua` owns host UI, `lib/document_io.lua` owns Sprite lifecycle,
  and `lib/workflows.lua` owns import/export orchestration. The adapter does
  not duplicate PSD parsing or normalized conversion semantics.
- The parser owns PSD decoding; the writer must not reinterpret PSD descriptors.
- `NormalizedDocument::find_layer` is the single crate-private source-layer
  traversal used by planning, writing, and read-back validation.
- Writer initialization owns the canvas, timeline, loop tag, and initial
  warnings. Preserve and planned layer/cel emission stay separate because their
  topology contracts differ. Read-back validation shares header checks but keeps
  preserve and planned topology checks separate.
- A conversion transaction owns temporary output until read-back validation and
  atomic commit complete.

The PSD export reader accepts two Aseprite-owned snapshots: an untouched
original for editable layer/cel data and a separately flattened copy for the
trusted composite. It never recomposites the original layer tree. The exporter
maps cels to static PSD pixel layers plus frame-local visibility, position, and
opacity metadata, then validates both the ag-psd container and the normalized
animation/composite before committing. Because the Minerva ag-psd fork does not
yet write `shmd`, the writer has one bounded post-processor for that missing block;
it uses ag-psd descriptor primitives and does not introduce another document
model.

Exports also append an optional private `p2rt` layer additional-info block to
materialized cel wrappers and variants. The block contains only a version,
logical layer ID, wrapper/variant role, and variant ordinal. It is not a visible
watermark or tracking payload. The extension keeps this metadata enabled by
default and stores the user's opt-out in `plugin.preferences`; marked inputs can
default to automatic association while ordinary PSD inputs retain preserve
semantics. Invalid or incomplete markers are ignored safely.

The current writer is deliberately experimental. `convert` writes to a
same-directory temporary file, reads it back through `aseprite-io`, and only
then commits it. The first coordinate policy uses `pixels.left/top` plus
frame-local PSD offsets as cel origins and is reported as provisional;
Photoshop coordinate equivalence is not claimed until visual review supplies
evidence.
