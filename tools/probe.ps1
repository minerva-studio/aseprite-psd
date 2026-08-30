param(
    [string] $InputPath = $env:PSD2ASE_FIXTURE
)

$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($InputPath)) {
    $InputPath = 'path\to\fixture.psd'
}

$ProbeDirectory = Join-Path $Root '.probe'
$RustOutput = Join-Path $ProbeDirectory 'rust-snapshot.json'
$OracleOutput = Join-Path $ProbeDirectory 'oracle-snapshot.json'
$OracleDirectory = Join-Path $Root 'tools\ag-psd-oracle'

if (-not (Test-Path -LiteralPath $InputPath -PathType Leaf)) {
    throw "PSD input is not a file: $InputPath"
}

$Before = Get-FileHash -LiteralPath $InputPath -Algorithm SHA256
$BeforeLength = (Get-Item -LiteralPath $InputPath).Length

& cargo run --locked --manifest-path (Join-Path $Root 'tools\rust-psd-probe\Cargo.toml') -- `
    --input $InputPath --output $RustOutput
if ($LASTEXITCODE -ne 0) {
    throw "Rust PSD probe failed with exit code $LASTEXITCODE"
}

& npm --prefix $OracleDirectory run oracle -- `
    --input $InputPath --output $OracleOutput
if ($LASTEXITCODE -ne 0) {
    throw "TypeScript ag-psd oracle failed with exit code $LASTEXITCODE"
}

$After = Get-FileHash -LiteralPath $InputPath -Algorithm SHA256
$AfterLength = (Get-Item -LiteralPath $InputPath).Length
if ($Before.Hash -ne $After.Hash -or $BeforeLength -ne $AfterLength) {
    throw "PSD input changed during probe execution"
}
Write-Host "input unchanged: $($After.Hash) ($AfterLength bytes)"

& node (Join-Path $Root 'tools\compare-probes.mjs') `
    --rust $RustOutput --oracle $OracleOutput
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}

Write-Host "probe outputs: $RustOutput and $OracleOutput"
