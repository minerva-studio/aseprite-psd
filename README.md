# psd2ase

`psd2ase` is a planned standalone converter from Photoshop PSD documents to
Aseprite documents. The project is currently in phase one: the Rust workspace,
public core boundary, and metadata inspection probe are being established.

The release implementation is intended to be native Rust so the final artifact
is one executable with no user-side runtime dependency. TypeScript `ag-psd`
remains a development-only oracle for differential validation; it will not be
included in the release binary.

## Current status

- `psd2ase --version` and `psd2ase --help` are available.
- `psd2ase inspect INPUT.psd` exercises the PSD parser without writing output.
- `psd2ase convert INPUT.psd` is intentionally gated until the parser
  compatibility probe and Aseprite writer validation pass.
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

The first real implementation gate is a differential probe against a supplied
PSD fixture. It must compare canvas metadata, the complete layer tree, layer
properties, and per-layer pixel hashes before conversion writing is enabled.
