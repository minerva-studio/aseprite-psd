# psd2ase

`psd2ase` is a standalone converter from Photoshop PSD documents to Aseprite
documents. The project is currently in phase six: the normalized reader,
minimal writer, and experimental cross-frame logical-layer association are
being validated.

The release implementation is intended to be native Rust so the final artifact
is one executable with no user-side runtime dependency. TypeScript `ag-psd`
remains a development-only oracle for differential validation; it will not be
included in the release binary.

## Current status

- `psd2ase --version` and `psd2ase --help` are available.
- `psd2ase inspect INPUT.psd` exercises the PSD parser without writing output.
- `psd2ase convert INPUT.psd [-o OUTPUT] [--overwrite]` writes a validated
  experimental Aseprite output through the normalized model.
- `psd2ase convert INPUT.psd --layer-association auto` enables experimental
  cross-frame logical-layer association; the default preserves the PSD source
  tree.
- Auto association uses stable track order by default. Use
  `--z-order auto` explicitly to enable experimental per-cel Z-Index changes;
  `--z-order auto` requires `--layer-association auto`.
- Stable ordering defaults to cross-frame overlap consensus. Use
  `--stable-order anchor` for the legacy anchor-frame order, or
  `--stable-order strict` to fail when overlapping order evidence is unresolved.
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
and using `pixels.left/top` plus frame-local PSD offsets as a provisional
cel-origin policy. It must be
reviewed in Aseprite before coordinate semantics are considered final.
