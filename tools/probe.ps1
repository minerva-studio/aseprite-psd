param(
    [string] $InputPath = $env:ASEPRITE_PSD_FIXTURE,
    [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($InputPath)) {
    throw 'PSD input is required: pass -InputPath or set ASEPRITE_PSD_FIXTURE'
}

$InputFile = (Resolve-Path -LiteralPath $InputPath -ErrorAction Stop).Path
$ProbeDirectory = if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    Join-Path $Root 'target\probe'
} elseif ([System.IO.Path]::IsPathRooted($OutputDirectory)) {
    $OutputDirectory
} else {
    Join-Path (Get-Location).Path $OutputDirectory
}
$null = New-Item -ItemType Directory -Force -Path $ProbeDirectory
$RustOutput = Join-Path $ProbeDirectory 'rust-snapshot.json'
$OracleOutput = Join-Path $ProbeDirectory 'oracle-snapshot.json'
$OracleDirectory = Join-Path $Root 'tools\ag-psd-oracle'
$RustManifest = Join-Path $Root 'tools\rust-aseprite-psd-probe\Cargo.toml'
$Comparator = Join-Path $Root 'tools\compare-probes.mjs'

if (-not (Test-Path -LiteralPath $InputFile -PathType Leaf)) {
    throw "PSD input is not a file: $InputFile"
}

$Before = Get-FileHash -LiteralPath $InputFile -Algorithm SHA256
$BeforeLength = (Get-Item -LiteralPath $InputFile).Length

& cargo run --locked --manifest-path $RustManifest -- `
    --input $InputFile --output $RustOutput
if ($LASTEXITCODE -ne 0) {
    throw "Rust PSD probe failed with exit code $LASTEXITCODE"
}

& npm --prefix $OracleDirectory run oracle -- `
    --input $InputFile --output $OracleOutput
if ($LASTEXITCODE -ne 0) {
    throw "TypeScript ag-psd oracle failed with exit code $LASTEXITCODE"
}

$After = Get-FileHash -LiteralPath $InputFile -Algorithm SHA256
$AfterLength = (Get-Item -LiteralPath $InputFile).Length
if ($Before.Hash -ne $After.Hash -or $BeforeLength -ne $AfterLength) {
    throw "PSD input changed during probe execution"
}
Write-Host "input unchanged: $($After.Hash) ($AfterLength bytes)"

& node $Comparator `
    --rust $RustOutput --oracle $OracleOutput
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host "probe outputs: $RustOutput and $OracleOutput"
