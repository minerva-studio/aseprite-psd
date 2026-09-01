# Development workflow

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
cargo run -p psd2ase -- --version
cargo run -p psd2ase -- --help
npm --prefix tools/ag-psd-oracle install --ignore-scripts --allow-git=all --cache tools/ag-psd-oracle/.npm-cache
pwsh -File tools/probe.ps1
```

Use a real PSD supplied outside the repository for compatibility testing. Do
not add customer artwork or private fixtures to Git.

## Test layout

Unit tests are centralized under each crate's `src/tests/` directory and are
split by the production owner (`core`, `layer_names`, `aseprite_writer`,
`logical_layers`, and `photoshop_animation`). Production modules only declare
their test module with `#[cfg(test)]` and a `#[path = "tests/..."]` attribute;
the tests therefore retain private access without scattering test bodies beside
production code. The CLI follows the same convention under
`crates/psd2ase-cli/src/tests/`. Crate-level `tests/` directories are reserved
for future black-box integration tests that exercise only public APIs.

The probe runner reads `path\to\fixture.psd` by default. Set
`PSD2ASE_FIXTURE` or pass `-InputPath` to select another local fixture. It
writes only ignored JSON snapshots under `.probe/`, verifies the source file's
size and SHA-256 before and after the run, and never creates an Aseprite file.

Stage 3 adds a Photoshop frame-animation gate to the same probe command. The
Rust scanner reads bounded image-resource and layer additional-info sections
(4000/4003, shmd/mlst/mdyn) and converts them into the format-independent
animation model in psd2ase-core. The TypeScript `ag-psd` master snapshot is
pinned as the development oracle for overlapping parser behaviour; the
Minerva-maintained Rust `ag-psd` fork remains the product runtime and includes
documented extensions beyond that oracle.

Stage 4 exposes `psd2ase_core::normalize` as the reader boundary. It owns the
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

- `psd2ase-core` owns normalized document semantics, pixel ownership, and
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
- `psd2ase` owns argument parsing, output policy, exit codes, and presentation.
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
