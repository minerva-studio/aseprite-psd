param(
    [Parameter(Mandatory = $true)]
    [string] $InputPath,
    [Parameter(Mandatory = $true)]
    [string] $OutputDirectory,
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 10000)]
    [int] $FrameCount,
    [string] $AsepritePath = 'aseprite'
)

$ErrorActionPreference = 'Stop'
$InputFile = (Resolve-Path -LiteralPath $InputPath -ErrorAction Stop).Path
$AsepriteCommand = Get-Command $AsepritePath -ErrorAction Stop
$AsepriteFile = $AsepriteCommand.Source
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$null = New-Item -ItemType Directory -Force -Path $OutputDirectory

$existingFrames = Get-ChildItem -LiteralPath $OutputDirectory -Filter 'frame-*.png' -File
if ($existingFrames.Count -gt 0) {
    throw "render output directory already contains frame PNGs: $OutputDirectory"
}

for ($index = 0; $index -lt $FrameCount; $index++) {
    $framePath = Join-Path $OutputDirectory "frame-$index.png"
    & $AsepriteFile -b $InputFile --frame-range "$index,$index" --save-as $framePath
    $exitCode = $LASTEXITCODE
    if ($null -ne $exitCode -and $exitCode -ne 0) {
        throw "Aseprite failed to render frame $index with exit code $exitCode"
    }
    for ($attempt = 0; $attempt -lt 20 -and -not (Test-Path -LiteralPath $framePath -PathType Leaf); $attempt++) {
        Start-Sleep -Milliseconds 100
    }
    if (-not (Test-Path -LiteralPath $framePath -PathType Leaf)) {
        throw "Aseprite did not create the expected frame: $framePath"
    }
}

Write-Host "rendered $FrameCount frames to $OutputDirectory"
