# User manual

[简体中文](user-guide.zh-CN.md) · [README](../README.md) · [Options reference](options.md)

Choose a workflow by PSD structure, then expand the setup instructions. Automatic association remains experimental: these are starting points, and playback and layer relationships need review after import.

## How is your PSD organized?

| Structure and goal | Workflow |
| --- | --- |
| Layered illustration whose layers and groups should stay intact | [Layered illustration](#illustration) |
| Photoshop frame-animation timeline switches layer visibility | [Timeline frame animation](#timeline) |
| Each layer or folder represents a frame | [Layer or folder animation](#hierarchy) |
| Extension-exported PSD with Aseprite relationships to restore | [Reimport](#roundtrip) |

Not installed yet? Start with the [README quick start](../README.md).

<a id="illustration"></a>

## Layered illustration

**Recognize it:** Layers and folders represent parts of a picture, without an intended frame sequence.

**Recommended approach:** Import a static document and preserve source-layer identities. This produces one Aseprite frame with supported layers and groups available for editing. Many folders do not imply many animation frames.

<details>
<summary>How to set it up</summary>

- Set `Frame source` to `Static document`.
- Select `Preserve layers` under `Layer association` and uncheck `Use metadata`.
- Leave `Jitter repair` off.

</details>

If layers represent different moments, use an animation workflow instead.

<a id="timeline"></a>

## Timeline frame animation

**Recognize it:** Photoshop's frame-animation timeline determines each frame's visible state. Different poses of the same character or part may occupy separate layers or groups, with visibility changing during playback.

**Recommended approach:** Read frames from the timeline, then try organizing corresponding content into feature tracks with stable stacking relationships. This suits documents you want to continue editing by part in Aseprite.

<details>
<summary>How to set it up: organize parts across frames</summary>

- Set `Frame source` to `Photoshop timeline`; `Frame layer depth` is unused here.
- Set `Layer association` to `Automatic association`.
- Uncheck `Use metadata`, then choose `Feature tracks` under `Association strategy`.
- Set `Z-order` to `stable` and `Stable order` to `consensus`.
- Leave `Preserve Photoshop metadata` and `Link identical cels` unchecked, and `Jitter repair` off.
- `Uncertain layers` is unavailable for this strategy and needs no adjustment.

This is a suggested starting point for this structure, not a guarantee of recovering the intended logical layers in every file.

</details>

**Review the result:** Check frame count, timing, part-to-track relationships, and overlaps. If parts change their front-to-back relationship across frames, consider per-frame stacking changes in the [options reference](options.md). If association does not match your editing intent, try conservative association or preserve source layers as a comparison. Without a real frame-animation timeline, use the next workflow.

<a id="hierarchy"></a>

## Layer or folder animation

**Recognize it:** The layer tree represents the frame sequence: each top-level layer may be a complete image, or an action folder may contain frame subgroups. Names alone do not establish frame boundaries.

**Recommended approach:** Explicitly select the hierarchy level that represents frames, then check the sequence so body parts are not mistaken for moments in time. Automatic frame detection does not turn multiple groups into animation by itself.

<details>
<summary>How to set it up</summary>

- Set `Frame source` to `Layer hierarchy`.
- Set `Frame layer depth` to the frame level: top-level items are `0`, their immediate children are `1`.
- For an initial check, select `Preserve layers` and uncheck `Use metadata` to inspect frame interpretation first; try automatic association afterward if parts need organizing across frames.
- Leave `Jitter repair` off.

</details>

If the timeline defines playback, use the timeline workflow. Mixed structures containing timeline-controlled content need an output check; child-folder count alone does not establish frame count.

<a id="roundtrip"></a>

## Reimport an extension export

**Recognize it:** This extension exported the file with converter-owned layer/frame relationships intact.

**Recommended approach:** Prefer saved relationships over inferring them again. This requires valid metadata and does not promise lossless round trips for every Photoshop feature.

<details>
<summary>How to set it up</summary>

- Set `Frame source` to `Automatic`.
- Select `Automatic association` and check `Use metadata`.
- Uncheck `Use metadata` to choose an association strategy yourself.
- If `PSD Metadata Recovery` appears, choose automatic association, preserve layers, or cancel; review any recovered result.

</details>

Unmarked and legacy files follow fallback paths described below. `Preserve Photoshop metadata` is a separate setting, not a substitute for `Use metadata`.

## Review and save

1. Play the animation and check frame count, timing, missing content, and overlaps.
2. Expand the layer tree and check that parts requiring independent edits remain independent.
3. Review paths and frames in any information-loss notice; use `Export Full Report...` to save the full report.
4. Press Ctrl+S or use Save As to save an `.aseprite` working file.

## Troubleshooting

| Symptom | Next step |
| --- | --- |
| Only one frame | Check for a frame-animation timeline; layer-based animation needs an explicit hierarchy selection |
| Association controls are disabled | Uncheck Use metadata in automatic mode; some controls apply only to specific strategies |
| Layers are not organized as intended | Try conservative association or preserve source layers for comparison |
| Incorrect overlaps | Check whether the source changes stacking order across frames before trying per-frame Z-order |
| You want fewer duplicate cels | Enable linked cels after checking association; editing linked content can affect other frames |
| Suspected specks or color flicker | Use jitter reporting before deciding to repair; repair changes pixels |

## Export and file saving

Use **File > Export > Export PSD/PSB...** to choose a destination, and keep `.aseprite` as your working file. See the [options reference](options.md) for empty layers, content reuse, and advanced settings. Experimental controls in current source are not a compatibility promise for released packages; version limits follow below.

<details>
<summary>Detailed import, export, and metadata behavior</summary>

The explicit Import command opens a modified document that is not associated with the temporary conversion file. Once the native integration is available, `File > Open` returns a document associated with the original PSD. For an explicit import, press Ctrl+S or use Save As to choose the final `.aseprite` path; Aseprite suggests the PSD's directory and base name.

The explicit Import command—and native `File > Open` once available—shows the same import options. Choose `Automatic association` or `Preserve layers`. In Automatic association mode, `Use metadata` selects the exact metadata preset; when it is off, the dialog exposes the experimental association controls and uses the normal heuristics. Legacy v1 and unmarked files use the automatic association fallback, while damaged converter metadata opens a recovery choice instead of being silently ignored. In particular, an unmarked PSD is intentionally not treated as `Preserve layers`: it falls back to the standard Automatic association path. Turn off `Use metadata` when you need to tune the association strategy for such a file.

Exports include an invisible, versioned PSD metadata block by default. It records only the metadata version, logical layer IDs, and materialized cel relationships; it does not contain file paths, usernames, device information, or usage tracking. Photoshop and other readers may ignore this block. Use **File > Export > PSD/PSB Support Settings...** to control both export embedding and import usage. Disabling import usage keeps Automatic association on the heuristic path even when metadata is present. Disabling export embedding leaves the PSD readable, but future opens cannot use exact converter-owned layer association from that file.

Once native integration is available, cancelling its `File > Open` import dialog reports `PSD opening cancelled by user.` so that a cancelled open is never confused with a failed or partially initialized document.

To export now, choose **File > Export > Export PSD/PSB...**. Native `File > Save As...` with a `.psd` or `.psb` destination becomes an additional entry point once the Aseprite integration is available. The extension snapshots isolated original and flattened copies, runs the bundled converter, validates the Photoshop document, and only then writes it through Aseprite's custom-format save stream. The save options let you choose whether empty pixel layers are included, and Ctrl+S reuses the selected format and option. Extension exports use Photoshop-compatible RLE channel compression automatically. Exporting an Aseprite timeline as a Photoshop timeline is not supported in 0.3.1; the supported export contract is a static layered PSD/PSB document.

Aseprite may open and truncate a native custom-format destination before the save callback runs. The extension validates the complete PSD before writing, but cannot provide transactional rollback for a failed overwrite. Use the explicit Export command to write a separate destination when the existing file must be preserved.

</details>

## Current boundaries

- Import packages have been validated with Aseprite 1.3.18.3 on Windows x64 and
Ubuntu/WSL2 Linux x64 with glibc. Native PSD/PSB `File > Open` and `File > Save As...` integration is expected in Aseprite 1.3.18.4. Earlier versions must use the extension's explicit Import and Export commands.
- macOS packages are built by the manual GitHub Actions workflow but have not
yet received authentic Aseprite runtime validation.
- The extension registers PSD/PSB custom-format load and save callbacks. The
explicit import command remains available for configurable import policies.
- Conversion preserves the normalized layer tree, RGBA8 cels, Photoshop frame
animation, and supported layer state. Logical-layer association and some coordinate mappings remain experimental, so important output should be reviewed in Aseprite.
- PSD 16/32-bits-per-channel input can be imported, but the parser down-converts
it to Aseprite RGBA8 and records `UnsupportedColor/Degraded` information loss. This PSD channel depth is distinct from Aseprite RGBA's 32 bits-per-pixel.
- PSD import preserves slice names, order, bounds, and static frame-0 keys.
Photoshop-only group, URL, target, message, alt text, background, outsets, and layer-association fields are recorded as `Slices/Degraded` information loss. Resource 1050 versions 6/7/8 have specification-driven tests; authentic Photoshop samples for versions 7/8 remain unverified.
- PSB input is supported. The pinned `psd-tools` `slices.psb` fixture has passed
Rust and TypeScript probe comparison plus conversion/read-back checks. Very large canvases remain limited by the dimensions Aseprite can represent, and performance near Photoshop's maximum dimensions has not been validated.
- Export preserves supported groups and static layer properties. Exporting an
Aseprite timeline as a Photoshop timeline is not supported in 0.3.1. Tilemaps use the independently flattened composite snapshot and are reported as rasterized; tag names/boundaries, slices, color profiles, and per-cel Z-Index are reported when they cannot remain editable.
- Small, deterministic PSD/PSB fixtures used by automated tests live under
`tests/fixtures/`; customer artwork and large/private documents are intentionally kept out of the repository.
