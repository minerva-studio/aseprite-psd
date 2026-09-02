#!/usr/bin/env pwsh
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('windows-x64', 'linux-x64', 'macos-arm64', 'macos-x64')]
    [string] $Platform,

    [string] $Binary,

    [switch] $NoBuild,

    [string] $Output
)

$ErrorActionPreference = 'Stop'

$scriptDirectory = Split-Path -Parent $MyInvocation.MyCommand.Path
$repoRoot = (Resolve-Path (Join-Path $scriptDirectory '..')).Path
$sourceDirectory = Join-Path $repoRoot 'extensions\aseprite-psd'
$moduleFiles = @('process.lua', 'dialogs.lua', 'document_io.lua', 'workflows.lua')

# Writes an error message and exits with the requested process code.
function Fail([string] $Message, [int] $Code) {
    [Console]::Error.WriteLine("error: $Message")
    exit $Code
}

# Runs a native process and converts a non-zero exit code into an exception.
function Invoke-Native([string] $FilePath, [string[]] $Arguments) {
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code $LASTEXITCODE`: $FilePath $($Arguments -join ' ')"
    }
}

if (-not (Test-Path -LiteralPath (Join-Path $sourceDirectory 'package.json') -PathType Leaf)) {
    Fail 'package.json not found' 66
}
if (-not (Test-Path -LiteralPath (Join-Path $sourceDirectory 'aseprite-psd.lua') -PathType Leaf)) {
    Fail 'aseprite-psd.lua not found' 66
}
foreach ($moduleFile in $moduleFiles) {
    $modulePath = Join-Path $sourceDirectory "lib\$moduleFile"
    if (-not (Test-Path -LiteralPath $modulePath -PathType Leaf)) {
        Fail "extension module not found: $modulePath" 66
    }
}

if ($Platform -ne 'windows-x64') {
    Fail 'the PowerShell entry point only builds windows-x64; use the Bash entry point for Unix platforms' 64
}

if ($NoBuild -and [string]::IsNullOrWhiteSpace($Binary)) {
    Fail '-Binary is required when -NoBuild is used' 64
}
if (-not $NoBuild) {
    $Binary = Join-Path $repoRoot 'target\release\aseprite-psd.exe'
    Write-Host "building release converter: $Binary"
    try {
        Invoke-Native 'cargo' @('build', '--release', '--locked', '-p', 'aseprite-psd')
    } catch {
        Fail $_.Exception.Message 1
    }
}

if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
    Fail "converter binary not found: $Binary" 66
}

if ([string]::IsNullOrWhiteSpace($Output)) {
    $Output = Join-Path $repoRoot 'dist\aseprite-psd-windows-x64.aseprite-extension'
}
$outputDirectory = Split-Path -Parent $Output
if ([string]::IsNullOrWhiteSpace($outputDirectory)) {
    $outputDirectory = (Get-Location).Path
}
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null
$Output = [IO.Path]::GetFullPath($Output)

$staging = Join-Path ([IO.Path]::GetTempPath()) "aseprite-psd-$([Guid]::NewGuid().ToString('N'))"
try {
    New-Item -ItemType Directory -Force -Path (Join-Path $staging 'bin\windows-x64'), (Join-Path $staging 'lib') | Out-Null
    Copy-Item -LiteralPath (Join-Path $sourceDirectory 'package.json') -Destination (Join-Path $staging 'package.json')
    Copy-Item -LiteralPath (Join-Path $sourceDirectory 'aseprite-psd.lua') -Destination (Join-Path $staging 'aseprite-psd.lua')
    foreach ($moduleFile in $moduleFiles) {
        Copy-Item -LiteralPath (Join-Path $sourceDirectory "lib\$moduleFile") -Destination (Join-Path $staging "lib\$moduleFile")
    }
    Copy-Item -LiteralPath $Binary -Destination (Join-Path $staging 'bin\windows-x64\aseprite-psd.exe')

    if (Test-Path -LiteralPath $Output) {
        Remove-Item -LiteralPath $Output -Force
    }
    Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $Output -CompressionLevel Optimal

    $archive = [IO.Compression.ZipFile]::OpenRead($Output)
    try {
        $requiredEntries = @('package.json', 'aseprite-psd.lua') + ($moduleFiles | ForEach-Object { "lib/$_" }) + 'bin/windows-x64/aseprite-psd.exe'
        $entryNames = @($archive.Entries | ForEach-Object { $_.FullName.Replace('\', '/') })
        foreach ($requiredEntry in $requiredEntries) {
            if ($entryNames -notcontains $requiredEntry) {
                throw "created archive is missing: $requiredEntry"
            }
        }
    } finally {
        $archive.Dispose()
    }
    Write-Host "created $Output"
} catch {
    Fail $_.Exception.Message 1
} finally {
    if (Test-Path -LiteralPath $staging) {
        Remove-Item -LiteralPath $staging -Recurse -Force
    }
}
