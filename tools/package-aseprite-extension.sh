#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: package-aseprite-extension.sh --platform PLATFORM [--binary PATH] [--output PATH]

PLATFORM must be linux-x64 or windows-x64.

When --binary is omitted, the script builds the native release converter before
packaging it. Use --no-build with --binary when the converter was built elsewhere.
EOF
}

platform=""
binary=""
output=""
build_binary=1

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
      build_binary=0
      shift 2
      ;;
    --no-build)
      build_binary=0
      shift
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
    executable_name="aseprite-psd"
    ;;
  windows-x64)
    executable_name="aseprite-psd.exe"
    ;;
  *)
    echo "error: --platform must be linux-x64 or windows-x64" >&2
    exit 64
    ;;
esac

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd -- "$script_dir/.." && pwd)"
source_dir="$repo_root/extensions/aseprite-psd"

[[ -f "$source_dir/package.json" ]] || { echo "error: package.json not found" >&2; exit 66; }
[[ -f "$source_dir/aseprite-psd.lua" ]] || { echo "error: aseprite-psd.lua not found" >&2; exit 66; }
module_files=(process.lua dialogs.lua document_io.lua workflows.lua)
for module_file in "${module_files[@]}"; do
  [[ -f "$source_dir/lib/$module_file" ]] || {
    echo "error: extension module not found: $source_dir/lib/$module_file" >&2
    exit 66
  }
done

if [[ "$build_binary" -eq 1 ]]; then
  case "$platform" in
    linux-x64)
      binary="$repo_root/target/release/aseprite-psd"
      ;;
    windows-x64)
      binary="$repo_root/target/release/aseprite-psd.exe"
      ;;
  esac
  echo "building release converter: $binary"
  (cd "$repo_root" && cargo build --release --locked -p aseprite-psd)
fi

[[ -n "$binary" ]] || { echo "error: --binary is required when --no-build is used" >&2; exit 64; }
[[ -f "$binary" ]] || { echo "error: converter binary not found: $binary" >&2; exit 66; }

if [[ -z "$output" ]]; then
  output="$repo_root/dist/aseprite-psd-$platform.aseprite-extension"
fi
output_dir="$(dirname -- "$output")"
mkdir -p "$output_dir"
output_dir="$(cd -- "$output_dir" && pwd)"
output="$output_dir/$(basename -- "$output")"

staging="$(mktemp -d "${TMPDIR:-/tmp}/aseprite-psd.XXXXXX")"
cleanup() {
  rm -rf -- "$staging"
}
trap cleanup EXIT

mkdir -p "$staging/bin/$platform"
mkdir -p "$staging/lib"
cp -- "$source_dir/package.json" "$staging/package.json"
cp -- "$source_dir/aseprite-psd.lua" "$staging/aseprite-psd.lua"
for module_file in "${module_files[@]}"; do
  cp -- "$source_dir/lib/$module_file" "$staging/lib/$module_file"
done
cp -- "$binary" "$staging/bin/$platform/$executable_name"

python_command=""
for candidate in python3 python; do
  if command -v "$candidate" >/dev/null 2>&1 \
    && "$candidate" -c 'import zipfile' >/dev/null 2>&1; then
    python_command="$candidate"
    break
  fi
done

quote_powershell_literal() {
  local value="$1"
  value="${value//\'/\'\'}"
  printf "'%s'" "$value"
}

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
elif command -v powershell.exe >/dev/null 2>&1 && command -v cygpath >/dev/null 2>&1; then
  windows_staging="$(cygpath -w "$staging")"
  windows_output="$(cygpath -w "$output")"
  powershell_script="\$staging = $(quote_powershell_literal "$windows_staging"); \$output = $(quote_powershell_literal "$windows_output"); Compress-Archive -Path (Join-Path \$staging '*') -DestinationPath \$output -CompressionLevel Optimal -Force"
  powershell.exe -NoLogo -NoProfile -NonInteractive -Command "$powershell_script"
else
  echo "error: install zip, Python 3, or PowerShell to create the extension archive" >&2
  exit 69
fi

echo "created $output"
