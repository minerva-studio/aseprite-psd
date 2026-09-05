# Options reference

[简体中文](options.zh-CN.md) · [User manual](user-guide.md) · [README](../README.md)

Choose a workflow in the manual first. These controls describe current source; installed packages may lag behind it.

## UI quick reference

| Control | Purpose / CLI mapping |
| --- | --- |
| Frame source | Automatic → `auto`; Photoshop timeline → `timeline` (requires frame-animation data); Static document → `static`; Layer hierarchy → `layer-depth:N` |
| Frame layer depth | Hierarchy only: top-level items are 0, immediate children are 1 |
| Layer association | Preserve layers → `preserve`; Automatic association without metadata → `auto` |
| Use metadata | Selects the `roundtrip` path and disables advanced association controls |
| Preserve Photoshop metadata | `--preserve-photoshop-metadata`; separate from converter relationship recovery |
| Association strategy | conservative prioritizes editable identities; compact prioritizes fewer tracks; Feature tracks → `feature`, organizing cross-frame feature relationships |
| Z-order | stable uses stable track order; auto permits experimental per-cel Z-Index |
| Stable order | consensus uses cross-frame overlap evidence; anchor uses an anchor frame; strict rejects unresolved order evidence |
| Uncertain layers | conservative only; group creates candidate folders, flat does not |
| Link identical cels | Automatic association without metadata only; `--linked-cels identical` |
| Jitter repair | UI offers off / report / repair; CLI also offers assist. Repair changes pixels |
| Export empty pixel layers | Checked → `--empty-layers include`; unchecked → omit |
| Content reuse | Frame folders → none; Reuse Linked Cels only → linked; Merge identical content → aggressive. The latter two are experimental; edits to shared content may affect multiple frames |

## Command line

Build the native CLI with Rust 1.88 or newer:

```text
cargo build --release --locked -p aseprite-psd
```

The export command accepts `--compression raw|rle|zip|zip-prediction` and `--empty-layers include|omit`. Compression defaults to Photoshop-compatible RLE, while empty layers default to `omit`. ZIP modes remain available for diagnostics but are outside the Photoshop compatibility target. With `omit`, each frame filters pixel layers that have no cel, zero cel opacity, or no non-transparent RGBA pixels; empty groups are pruned as well. Hidden layers with non-transparent pixels remain editable, and `include` preserves the complete empty/transparent state layout.

See the [changelog](../CHANGELOG.md) for release notes and [the development workflow](development.md) for testing, extension packaging, CI, and release instructions.

Inspect a PSD without writing output:

```text
aseprite-psd inspect INPUT.psd
```

Convert a PSD, refusing to replace an existing output unless `--overwrite` is specified:

```text
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --overwrite
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association roundtrip
aseprite-psd convert INPUT.psd -o OUTPUT.aseprite --layer-association auto --linked-cels identical --jitter-mode repair --jitter-kind all
```

Export an Aseprite snapshot using a separately flattened snapshot produced by Aseprite. The output extension selects PSD or PSB, and existing output is preserved unless `--overwrite` is explicit:

```text
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite
aseprite-psd export INPUT.aseprite -o OUTPUT.psb --composite COMPOSITE.aseprite --report REPORT.json --overwrite
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --roundtrip-metadata off
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --empty-layers omit
aseprite-psd export INPUT.aseprite -o OUTPUT.psd --composite COMPOSITE.aseprite --content-reuse linked
```

Run `aseprite-psd --help` for the complete command syntax.

Animated exports accept `--content-reuse none|linked|aggressive`. `none` keeps one physical frame folder per timeline frame. `linked` may share complete, identical frame-folder states only when the source Aseprite cels explicitly share a linked-cel target. `aggressive` also shares complete states with exact pixel and display-property equality. The timeline frame count and playback order are never shortened; these modes are experimental.

Frame interpretation is explicit for PSDs without a Photoshop timeline:

- `--frame-source auto` is the default. It uses a real Photoshop timeline when
present and otherwise keeps the PSD static.
- `--frame-source static` always imports one static frame.
- `--frame-source top-level` treats each top-level layer or group as a frame.
A top-level layer named `Background` is reported and shared by every frame. This mode is intended for explicitly confirmed layer-per-frame exports such as Procreate Animation Assist PSDs; the Procreate marker alone never enables it.

## Layer association

- `--layer-association preserve` is the default and preserves source-layer
identity.
- `--layer-association roundtrip` restores valid v2 frame-group metadata exactly,
uses automatic association for legacy v1 markers, preserves unmarked files, and exits with recovery-required status for damaged converter metadata. It intentionally rejects auto-only tuning flags.
- `--layer-association auto` defaults to the conservative planner, which
prioritizes editable logical identities.
- `--association-strategy compact` explicitly prioritizes the fewest tracks
that preserve the rendered result.
- `--association-strategy conservative` enables multilingual copy-family,
multi-track, and candidate-folder analysis. Ambiguous identities remain separate.

Automatic association does not require perfect layer names. Default Photoshop names, lazy naming, and names that drift between frames can still be resolved when cross-frame structure, mutual exclusion, pixels, positions, ordering, and names provide enough combined evidence. In those cases the solver restores a stable `layer × frame` logical track without requiring manual PSD renaming. If the evidence is insufficient, it keeps identities separate and reports the uncertainty instead of silently merging tracks.

- Stable track order uses cross-frame overlap consensus by default. Use
`--stable-order anchor` for anchor-frame ordering or `strict` to reject unresolved evidence.
- `--z-order auto` enables experimental per-cel Z-Index changes and requires
automatic association. Conservative mode also accepts `--uncertain-layers flat` to disable candidate folders.

`--linked-cels identical` enables lossless reuse of equal RGBA pixel buffers on the same automatically associated output layer. Positions, opacity, and per-cel Z-Index remain frame-local. The default is `off`; only exact size-and-byte matches are linked. It requires `--layer-association auto` because `preserve` emits each source layer independently and has no cross-layer cel reuse candidates.

## Import jitter repair

Jitter handling is disabled by default. `--jitter-mode report` only emits diagnostics, `assist` supplies stabilized comparison evidence to automatic association, and `repair` changes emitted cel pixels. Select `alpha`, `color`, or `all` with `--jitter-kind`, and choose the `conservative` or `balanced` threshold profile. Color repair is restricted to already-associated tracks with matching size and origin; it selects a real representative cel rather than synthesizing colors. Advanced overrides are available through `--jitter-alpha-threshold`, `--jitter-max-speck-area`, `--jitter-max-changed-ratio`, and `--jitter-max-channel-delta`.
