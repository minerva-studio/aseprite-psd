# Third-party licenses

This file records the dependency sources used by the project. Exact license
texts for a release artifact must be regenerated from the resolved Cargo lockfile
as part of the release audit.

| Dependency | Cargo package | Purpose | Upstream | License |
|---|---|---|---|---|
| ag-psd-rs | `ag-psd` | PSD/PSB parsing and pixel decoding | https://github.com/minerva-studio/ag-psd-rs (fork of https://github.com/Vasyanator/ag-psd-rs) | MIT |
| aseprite-io | `aseprite-io` | Aseprite file read/write | https://github.com/spebern/aseprite-io | MIT OR Apache-2.0 |

## Test fixtures

| Asset | Purpose | Source | License / attribution |
| --- | --- | --- | --- |
| `tests/fixtures/psb/psd-tools-slices/input.psb` | PSB v2 slices parser fixture | https://github.com/psd-tools/psd-tools at `6fb7bd5215069ed63cbe009e921c3f33aa97a3ec` | Upstream repository MIT; see the fixture `SOURCE.md` |

The repository names and published Cargo package names intentionally differ;
Cargo.toml uses the published names. The library namespace exported by
`aseprite-io` is `aseprite`.
