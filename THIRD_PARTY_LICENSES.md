# Third-party licenses

This file records the dependency sources used by the project. Exact license
texts for a release artifact must be regenerated from the resolved Cargo lockfile
as part of the release audit.

| Dependency | Cargo package | Purpose | Upstream | License |
|---|---|---|---|---|
| ag-psd-rs | `ag-psd` | PSD/PSB parsing and pixel decoding | https://github.com/minerva-studio/ag-psd-rs (fork of https://github.com/Vasyanator/ag-psd-rs) | MIT |
| aseprite-io | `aseprite-io` | Aseprite file read/write | https://github.com/spebern/aseprite-io | MIT OR Apache-2.0 |

The repository names and published Cargo package names intentionally differ;
Cargo.toml uses the published names. The library namespace exported by
`aseprite-io` is `aseprite`.
