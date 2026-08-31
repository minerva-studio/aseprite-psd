#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: package-aseprite-extension.sh --platform PLATFORM --binary PATH [--output PATH]

PLATFORM must be linux-x64 or windows-x64.
EOF
}

platform=""
binary=""
output=""

while (($# > 0)); do
  case "$1" in
    --platform)
      [[ $# -ge 2 ]] || { usage >&2; exit 64; }
      platform="$2"
      shift 2
      ;;
    --binary)
      [[ $# -ge 2 ]] || { usage >&2; exit 64; }
      binary="$2"
      shift 2
      ;;
    --output)
      [[ $# -ge 2 ]] || { usage >&2; exit 64; }
      output="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 64
      ;;
  esac
done

case "$platform" in
  linux-x64)
    executable_name="psd2ase"
    ;;
  windows-x64)
    executable_name="psd2ase.exe"
    ;;
  *)
    echo "error: --platform must be linux-x64 or windows-x64" >&2
    exit 64
    ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
source_dir="$repo_root/extensions/psd2ase-aseprite"

[[ -n "$binary" ]] || { echo "error: --binary is required" >&2; exit 64; }
[[ -f "$binary" ]] || { echo "error: converter binary not found: $binary" >&2; exit 66; }
[[ -f "$source_dir/package.json" ]] || { echo "error: package.json not found" >&2; exit 66; }
[[ -f "$source_dir/psd2ase.lua" ]] || { echo "error: psd2ase.lua not found" >&2; exit 66; }

if [[ -z "$output" ]]; then
  output="$repo_root/dist/psd2ase-aseprite-$platform.aseprite-extension"
fi
output_dir="$(dirname -- "$output")"
mkdir -p "$output_dir"
output_dir="$(cd -- "$output_dir" && pwd)"
output="$output_dir/$(basename -- "$output")"

staging="$(mktemp -d "${TMPDIR:-/tmp}/psd2ase-aseprite.XXXXXX")"
cleanup() {
  rm -rf -- "$staging"
}
trap cleanup EXIT

mkdir -p "$staging/bin/$platform"
cp -- "$source_dir/package.json" "$staging/package.json"
cp -- "$source_dir/psd2ase.lua" "$staging/psd2ase.lua"
cp -- "$binary" "$staging/bin/$platform/$executable_name"

python_command=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 \
    && "$candidate" -c 'import zipfile' >/dev/null 2>&1; then
    python_command="$candidate"
    break
  fi
done

if command -v zip >/dev/null 2>&1; then
  (cd "$staging" && zip -q -r "$output" .)
elif [[ -n "$python_command" ]]; then
  "$python_command" - "$staging" "$output" <<'PY'
import sys
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile

staging = Path(sys.argv[1])
output = Path(sys.argv[2])
with ZipFile(output, "w", ZIP_DEFLATED) as archive:
    for path in staging.rglob("*"):
        if path.is_file():
            archive.write(path, path.relative_to(staging).as_posix())
PY
else
  echo "error: install zip or Python 3 to create the extension archive" >&2
  exit 69
fi

echo "created $output"
