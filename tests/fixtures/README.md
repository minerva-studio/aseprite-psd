# Test fixtures

This directory contains small, deterministic input/output pairs used by repository tests and manual probe checks.

Each fixture directory uses the following contract:

- `input.psd` is the stable source document.
- `expected.aseprite` is the expected conversion shape used for round-trip inspection.

The files were promoted from the local `.probe/samples` set on 2026-09-02. The original local samples are intentionally
left in place; `.probe` remains an ignored workspace for generated reports and experiments.

## Fixture inventory

| Fixture | Purpose | Expected losses | Size (PSD / Aseprite) |
| --- | --- | ---: | ---: |
| `psd/single-layer` | Minimal PSD baseline | 6,476 / 22,420 bytes |
| `psd/two-layer` | Layer composition baseline | 14,176 / 38,118 bytes |
| `psd/high-resolution` | 32-bit source rejection boundary | 201,944 / 60,318 bytes |

Probe and conversion reports are generated locally and must not be committed.
They may contain machine-specific absolute paths.

These are test inputs, not release assets. Generated renders, probe snapshots, historical experiments, and manual
interoperability-review files belong under ignored `.probe/`, `target/`, or `dist/` directories.
