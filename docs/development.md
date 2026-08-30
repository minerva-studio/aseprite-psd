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

## Ownership rules

- `psd2ase-core` owns normalized document semantics and conversion invariants.
- `psd2ase` owns argument parsing, output policy, exit codes, and presentation.
- The parser owns PSD decoding; the writer must not reinterpret PSD descriptors.
- A conversion transaction owns temporary output until read-back validation and
  atomic commit complete.

The current `convert` entry point returns an explicit not-ready error. This is a
deliberate safety boundary, not a successful conversion path.
