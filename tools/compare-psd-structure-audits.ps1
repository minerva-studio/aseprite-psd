param(
    [Parameter(Mandatory = $true)]
    [string[]] $Photoshop,
    [Parameter(Mandatory = $true)]
    [string[]] $Generated,
    [Parameter(Mandatory = $true)]
    [string] $OutputDirectory
)

$ErrorActionPreference = 'Stop'

function Read-Audit([string] $Path) {
    Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}

function Get-LayerKeys($Audit) {
    @($Audit.layers | ForEach-Object { $_.additional_info } | ForEach-Object { $_.key } | Sort-Object -Unique)
}

function Get-DocumentKeys($Audit) {
    @($Audit.document_additional_info | ForEach-Object { $_.key } | Sort-Object -Unique)
}

function Get-ResourceClasses($Audit) {
    @($Audit.resources | ForEach-Object { "$($_.id):$($_.classification)" } | Sort-Object -Unique)
}

function Get-Intersection([object[]] $Sets) {
    if ($Sets.Count -eq 0) { return @() }
    $result = @($Sets[0])
    foreach ($set in $Sets | Select-Object -Skip 1) {
        $lookup = @{}; foreach ($value in $set) { $lookup[$value] = $true }
        $result = @($result | Where-Object { $lookup.ContainsKey($_) })
    }
    @($result | Sort-Object -Unique)
}

function Get-Union([object[]] $Sets) {
    @($Sets | ForEach-Object { $_ } | Sort-Object -Unique)
}

function Get-Missing([string[]] $Expected, [string[]] $Actual) {
    $lookup = @{}; foreach ($value in $Actual) { $lookup[$value] = $true }
    @($Expected | Where-Object { -not $lookup.ContainsKey($_) })
}

function Get-Counts($Audit) {
    $dividerLyid = 0; $dividerShmd = 0; $normalShmd = 0; $normalMlst = 0
    foreach ($layer in $Audit.layers) {
        foreach ($info in $layer.additional_info) {
            if ($layer.is_bounding_divider -and $info.key -eq 'lyid') { $dividerLyid++ }
            if ($info.key -eq 'shmd') {
                if ($layer.is_bounding_divider) { $dividerShmd++ } else { $normalShmd++ }
                if (-not $layer.is_bounding_divider) {
                    $normalMlst += @($info.metadata_records | Where-Object key -eq 'mlst').Count
                }
            }
        }
    }
    [ordered]@{
        resources = $Audit.resources.Count
        physical_layers = $Audit.layers.Count
        bounding_dividers = @($Audit.layers | Where-Object is_bounding_divider).Count
        divider_lyid = $dividerLyid
        divider_shmd = $dividerShmd
        normal_shmd = $normalShmd
        normal_mlst = $normalMlst
        frames = @($Audit.references.frame_ids | Sort-Object -Unique).Count
        composite = $Audit.composite.validation
    }
}

$photoshopAudits = @($Photoshop | ForEach-Object { Read-Audit $_ })
$generatedAudits = @($Generated | ForEach-Object { Read-Audit $_ })
$commonLayerKeys = Get-Intersection @($photoshopAudits | ForEach-Object { ,@(Get-LayerKeys $_) })
$commonDocumentKeys = Get-Intersection @($photoshopAudits | ForEach-Object { ,@(Get-DocumentKeys $_) })
$commonResources = Get-Intersection @($photoshopAudits | ForEach-Object { ,@(Get-ResourceClasses $_) })
$generatedLayerKeys = Get-Union @($generatedAudits | ForEach-Object { ,@(Get-LayerKeys $_) })
$generatedDocumentKeys = Get-Union @($generatedAudits | ForEach-Object { ,@(Get-DocumentKeys $_) })
$generatedResources = Get-Union @($generatedAudits | ForEach-Object { ,@(Get-ResourceClasses $_) })

$comparison = [ordered]@{
    schema_version = 1
    photoshop_common = [ordered]@{
        layer_additional_info = $commonLayerKeys
        document_additional_info = $commonDocumentKeys
        resources = $commonResources
    }
    generated_missing = [ordered]@{
        layer_additional_info = Get-Missing $commonLayerKeys $generatedLayerKeys
        document_additional_info = Get-Missing $commonDocumentKeys $generatedDocumentKeys
        resources = Get-Missing $commonResources $generatedResources
    }
    files = [ordered]@{}
    evidence = @(
        [ordered]@{ classification = 'Required candidate'; item = 'bounding divider lyid/shmd'; reason = 'present on every divider in all four Photoshop files and absent on every generated divider' },
        [ordered]@{ classification = 'Required candidate'; item = 'Patt and FMsk document blocks'; reason = 'present in all four Photoshop files and absent in all generated files; necessity remains unproven' },
        [ordered]@{ classification = 'Required candidate'; item = 'FrGA in AnDs'; reason = 'present in all four Photoshop animation catalogs and absent from every generated catalog' },
        [ordered]@{ classification = 'Correlated'; item = 'physical layer topology'; reason = 'Photoshop and generated files encode different layer/divider counts' },
        [ordered]@{ classification = 'Document metadata'; item = 'CAI /OCIO/GenI/cinf'; reason = 'absent from one Photoshop animation sample, so not a universal timeline condition' },
        [ordered]@{ classification = 'Opaque'; item = 'unknown resource and additional-info payloads'; reason = 'wire identity is recorded but semantics are not inferred' }
    )
}

foreach ($index in 0..($Photoshop.Count - 1)) { $comparison.files[$Photoshop[$index]] = Get-Counts $photoshopAudits[$index] }
foreach ($index in 0..($Generated.Count - 1)) { $comparison.files[$Generated[$index]] = Get-Counts $generatedAudits[$index] }

New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$jsonPath = Join-Path $OutputDirectory 'comparison.json'
$markdownPath = Join-Path $OutputDirectory 'README.md'
$comparison | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $jsonPath -Encoding utf8

$lines = @(
    '# Step 6 PSD structure audit',
    '',
    'The reports inventory wire-level blocks. A common Photoshop-only structure is a candidate, not proof of necessity.',
    '',
    '## Photoshop-common structures missing from generated files',
    '',
    "- Layer additional-info: $((Get-Missing $commonLayerKeys $generatedLayerKeys) -join ', ')",
    "- Document additional-info: $((Get-Missing $commonDocumentKeys $generatedDocumentKeys) -join ', ')",
    "- Image resources: $((Get-Missing $commonResources $generatedResources) -join ', ')",
    '',
    '## Strongest structural difference',
    '',
    'Every physical Photoshop layer record, including each bounding divider, has `lyid` and `shmd`; generated bounding dividers have neither. All ordinary layers in both families have `shmd/mlst`, so the unresolved difference is specifically the divider records and physical topology.',
    '',
    '## Resource-external findings',
    '',
    'All Photoshop files contain document-level `Patt` and `FMsk`; generated files contain no document-level additional-info. `CAI /OCIO/GenI/cinf` are not universal because the six-frame parrot file omits them.',
    '',
    'All four Photoshop `AnDs` descriptors contain `FrGA`; generated descriptors do not. Their nested descriptor classes and the `AFSt/FsID` relation otherwise match the inspected shape.',
    '',
    '## Limits',
    '',
    'No item is proven to be a Photoshop timeline recognition condition until a controlled Photoshop test succeeds. Opaque payloads are inventoried but are not candidates for blind copying.'
)
$lines | Set-Content -LiteralPath $markdownPath -Encoding utf8
Write-Output "wrote $jsonPath"
Write-Output "wrote $markdownPath"
