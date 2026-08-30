# psd2ase

`psd2ase` is a standalone converter from Photoshop PSD documents to Aseprite
documents. The project is currently in phase five: the normalized reader and a
minimal, reviewable Aseprite writer are being validated.

The release implementation is intended to be native Rust so the final artifact
is one executable with no user-side runtime dependency. TypeScript `ag-psd`
remains a development-only oracle for differential validation; it will not be
included in the release binary.

## Current status

- `psd2ase --version` and `psd2ase --help` are available.
- `psd2ase inspect INPUT.psd` exercises the PSD parser without writing output.
- `psd2ase convert INPUT.psd [-o OUTPUT] [--overwrite]` writes a validated
  experimental Aseprite output through the normalized model.
- No PSD or PSB fixtures are committed to this repository.

## Build

```text
cargo fmt --all -- --check
cargo test --workspace
cargo run -p psd2ase -- --version
```

The parser and writer dependencies are the crates published as `ag-psd` and
`aseprite-io`.
Their upstream repositories and license details are tracked in
`THIRD_PARTY_LICENSES.md`.

## Scope gate

The first writer output is experimental: it preserves the normalized layer
tree, RGBA8 cels, and animation frames, while reporting unsupported mappings
and using `pixels.left/top` as a provisional cel-origin policy. It must be
reviewed in Aseprite before coordinate semantics are considered final.
