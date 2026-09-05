#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: package-aseprite-extension.sh --platform PLATFORM [--binary PATH | --binary-dir PATH] [--output PATH]

PLATFORM must be linux-x64, macos-arm64, macos-x64, windows-x64, or universal.

When --binary is omitted, the script builds the native release converter before
packaging it. Use --no-build with --binary when the converter was built elsewhere.
Universal packaging requires --binary-dir containing one binary per platform and
never builds a converter itself.
EOF
}

platform=""
binary=""
binary_dir=""
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
    --binary-dir)
      [[ $# -ge 2 ]] || { usage >&2; exit 64; }
      binary_dir="$2"
      build_binary=0
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
    executable_name="aseprite-psd"
    ;;
  macos-arm64|macos-x64)
    executable_name="aseprite-psd"
    ;;
  windows-x64)
    executable_name="aseprite-psd.exe"
    ;;
  universal)
    executable_name=""
    ;;
  *)
    echo "error: --platform must be linux-x64, macos-arm64, macos-x64, windows-x64, or universal" >&2
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

if [[ "$platform" == "universal" ]]; then
  [[ -n "$binary_dir" ]] || {
    echo "error: --binary-dir is required for universal packaging" >&2
    exit 64
  }
  [[ -z "$binary" ]] || {
    echo "error: --binary and --binary-dir cannot be used together" >&2
    exit 64
  }
elif [[ -n "$binary_dir" ]]; then
  echo "error: --binary-dir is only valid with --platform universal" >&2
  exit 64
fi

if [[ "$build_binary" -eq 1 ]]; then
  case "$platform" in
    linux-x64)
      binary="$repo_root/target/release/aseprite-psd"
      ;;
    macos-arm64|macos-x64)
      binary="$repo_root/target/release/aseprite-psd"
      ;;
    windows-x64)
      binary="$repo_root/target/release/aseprite-psd.exe"
      ;;
  esac
  echo "building release converter: $binary"
  (cd "$repo_root" && cargo build --release --locked -p aseprite-psd)
fi

if [[ "$platform" != "universal" ]]; then
  [[ -n "$binary" ]] || { echo "error: --binary is required when --no-build is used" >&2; exit 64; }
  [[ -f "$binary" ]] || { echo "error: converter binary not found: $binary" >&2; exit 66; }
fi

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
  if [[ -n "${archive_validation_root:-}" ]]; then
    rm -rf -- "$archive_validation_root"
  fi
}
trap cleanup EXIT

if [[ "$platform" == "universal" ]]; then
  mkdir -p "$staging/bin"
else
  mkdir -p "$staging/bin/$platform"
fi
mkdir -p "$staging/lib"
cp -- "$repo_root/LICENSE-MIT" "$staging/LICENSE-MIT"
cp -- "$repo_root/LICENSE-APACHE" "$staging/LICENSE-APACHE"
cp -- "$source_dir/package.json" "$staging/package.json"
cp -- "$source_dir/aseprite-psd.lua" "$staging/aseprite-psd.lua"
for module_file in "${module_files[@]}"; do
  cp -- "$source_dir/lib/$module_file" "$staging/lib/$module_file"
done
if [[ "$platform" == "universal" ]]; then
  universal_platforms=(windows-x64 linux-x64 macos-arm64 macos-x64)
  for universal_platform in "${universal_platforms[@]}"; do
    if [[ "$universal_platform" == "windows-x64" ]]; then
      universal_executable="aseprite-psd.exe"
    else
      universal_executable="aseprite-psd"
    fi
    universal_binary="$binary_dir/$universal_platform/$universal_executable"
    [[ -f "$universal_binary" ]] || {
      echo "error: universal converter binary not found: $universal_binary" >&2
      exit 66
    }
    if [[ "$universal_platform" != "windows-x64" ]]; then
      chmod +x "$universal_binary"
    fi
    mkdir -p "$staging/bin/$universal_platform"
    cp -- "$universal_binary" "$staging/bin/$universal_platform/$universal_executable"
  done
else
  cp -- "$binary" "$staging/bin/$platform/$executable_name"
fi

archive_validation_root=""
if command -v zip >/dev/null 2>&1; then
  (cd "$staging" && zip -q -r "$output" .)
elif command -v powershell.exe >/dev/null 2>&1; then
  staging_windows="$(cygpath -w "$staging" 2>/dev/null || printf '%s' "$staging")"
  output_windows="$(cygpath -w "$output" 2>/dev/null || printf '%s' "$output")"
  powershell.exe -NoProfile -NonInteractive -Command \
    "\$staging='$staging_windows';\$output='$output_windows';Compress-Archive -Path (Join-Path \$staging '*') -DestinationPath \$output -Force"
  archive_validation_root="$(mktemp -d "${TMPDIR:-/tmp}/aseprite-psd-archive.XXXXXX")"
  validation_windows="$(cygpath -w "$archive_validation_root" 2>/dev/null || printf '%s' "$archive_validation_root")"
  powershell.exe -NoProfile -NonInteractive -Command \
    "\$archive='$output_windows';\$destination='$validation_windows';Expand-Archive -Path \$archive -DestinationPath \$destination -Force"
else
  echo "error: install zip or provide PowerShell Compress-Archive to create the extension archive" >&2
  exit 69
fi

if command -v unzip >/dev/null 2>&1; then
  archive_entries="$(unzip -Z1 "$output")"
else
  archive_entries=""
fi
require_entry() {
  local entry="$1"
  if [[ -n "$archive_validation_root" ]]; then
    [[ -f "$archive_validation_root/$entry" ]] && return 0
  elif printf '%s\n' "$archive_entries" | grep -Fqx "$entry"; then
    return 0
  fi
  if [[ -z "$archive_validation_root" && -z "$archive_entries" ]] || [[ -n "$archive_validation_root" ]]; then
    echo "error: created archive is missing: $entry" >&2
    exit 1
  fi
}
require_entry "LICENSE-MIT"
require_entry "LICENSE-APACHE"
require_entry "package.json"
require_entry "aseprite-psd.lua"
for module_file in "${module_files[@]}"; do
  require_entry "lib/$module_file"
done
if [[ "$platform" == "universal" ]]; then
  for universal_platform in "${universal_platforms[@]}"; do
    if [[ "$universal_platform" == "windows-x64" ]]; then
      universal_executable="aseprite-psd.exe"
    else
      universal_executable="aseprite-psd"
    fi
    require_entry "bin/$universal_platform/$universal_executable"
  done
else
  require_entry "bin/$platform/$executable_name"
fi

echo "created $output"
