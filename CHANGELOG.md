# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.3.1]: https://github.com/minerva-studio/aseprite-psd/compare/v0.3.0...v0.3.1
