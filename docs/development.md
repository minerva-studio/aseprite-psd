# Development workflow

## Phase gates

1. **Toolchain:** workspace, dependency versions, license records, and CI build
   matrix are healthy.
2. **PSD compatibility probe:** an unmodified parser is compared with the
   TypeScript oracle on a representative PSD before any writer work is enabled.
3. **Normalized model:** layer ownership, frame state, and pixel lifetime are
   explicit and format-independent.
4. **Aseprite writer:** output is written transactionally, read back, and
   structurally and visually validated before replacement.
5. **CLI and release:** exit codes, reports, platform artifacts, and manual
   Aseprite checks are complete.

## Local commands

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo run -p psd2ase -- --version
cargo run -p psd2ase -- --help
npm --prefix tools/ag-psd-oracle install --ignore-scripts --cache tools/ag-psd-oracle/.npm-cache
pwsh -File tools/probe.ps1
```

Use a real PSD supplied outside the repository for compatibility testing. Do
not add customer artwork or private fixtures to Git.

The probe runner reads `path\to\fixture.psd` by default. Set
`PSD2ASE_FIXTURE` or pass `-InputPath` to select another local fixture. It
writes only ignored JSON snapshots under `.probe/`, verifies the source file's
size and SHA-256 before and after the run, and never creates an Aseprite file.

Stage 3 adds a Photoshop frame-animation gate to the same probe command. The
Rust scanner reads bounded image-resource and layer additional-info sections
(4000/4003, shmd/mlst/mdyn) and converts them into the format-independent
animation model in psd2ase-core. The published ag-psd 0.2.0 remains unchanged;
TypeScript ag-psd is still only the development oracle.

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
  conversion invariants. Future PSD and Aseprite writers must use this model as
  their conversion boundary.
- `psd2ase` owns argument parsing, output policy, exit codes, and presentation.
- The parser owns PSD decoding; the writer must not reinterpret PSD descriptors.
- A conversion transaction owns temporary output until read-back validation and
  atomic commit complete.

The current writer is deliberately experimental. `convert` writes to a
same-directory temporary file, reads it back through `aseprite-io`, and only
then commits it. The first coordinate policy uses `pixels.left/top` plus
frame-local PSD offsets as cel origins and is reported as provisional;
Photoshop coordinate equivalence is not claimed until visual review supplies
evidence.
