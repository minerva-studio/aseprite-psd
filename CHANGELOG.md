# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.2] - 2026-09-05

### Added

- Export Aseprite animations as genuine Photoshop Frame Animation timelines.
  The writer emits the frame catalog and per-layer frame states required by
  Photoshop, preserving frame count, playback order, durations, loop policy,
  and the selected active frame.
- Add experimental animated-export content reuse. `linked` shares only complete
  frame states backed by explicitly linked Aseprite cels; `aggressive` can also
  share exactly identical displayed states. Both retain the logical timeline
  length and playback order.

### Changed

- Update the Aseprite export dialog and documentation to describe Photoshop
  Frame Animation export and its editing implications.

### Known limitations

- Aseprite tag names and boundaries are flattened into one deterministic
  Photoshop frame sequence; they are not retained as editable Photoshop tags.
- Content reuse is experimental. Editing shared Photoshop content can affect
  multiple logical frames.

## [0.3.1] - 2026-09-03

### Fixed

- Changed Aseprite extension and command-line exports to use Photoshop-compatible
  RLE channel compression by default, preventing ZIP-compressed PSD/PSB files
  that tolerant readers may accept but Photoshop may reject.
- Removed the compression selector from the Aseprite extension so it cannot
  accidentally create incompatible ZIP-compressed exports. The command-line
  ZIP modes remain available for diagnostics and now warn that they are outside
  the Photoshop compatibility target.

### Known limitations

- Export produces a static layered PSD/PSB document. Aseprite timelines cannot
  yet be recreated as Photoshop timelines.

[0.3.2]: https://github.com/minerva-studio/aseprite-psd/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/minerva-studio/aseprite-psd/compare/v0.3.0...v0.3.1
