param(
    [ValidatePattern('^\d+\.\d+\.\d+(\.\d+)?$')]
    [string] $Version
)

$ErrorActionPreference = 'Stop'

$repository = Split-Path $PSScriptRoot -Parent
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -eq $cargo) {
    $cargoPath = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
} else {
    $cargoPath = $cargo.Source
}
$plugin = Join-Path $repository 'stream-deck\com.2sbsi.audio-switch.sdPlugin'
$distribution = Join-Path $repository 'dist'
$buildTarget = Join-Path $repository 'target\stream-deck-package'
$manifestPath = Join-Path $plugin 'manifest.json'

if ([string]::IsNullOrWhiteSpace($Version)) {
    $metadata = & $cargoPath metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the Cargo workspace version.' }

    $application = $metadata.packages | Where-Object name -EQ 'audio-switch' | Select-Object -First 1
    if ($null -eq $application) { throw 'Could not find the audio-switch Cargo package.' }
    $Version = "$($application.version).0"
} elseif ($Version -match '^\d+\.\d+\.\d+$') {
    $Version = "$Version.0"
}

$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.Category -and -not $manifest.CategoryIcon) {
    throw 'Stream Deck manifests with Category must also define CategoryIcon.'
}
if ($manifest.CategoryIcon) {
    foreach ($suffix in '', '@2x') {
        $iconBase = Join-Path $plugin ($manifest.CategoryIcon + $suffix)
        $iconExists = Test-Path -LiteralPath ($iconBase + '.png') -PathType Leaf
        $iconExists = $iconExists -or (Test-Path -LiteralPath ($iconBase + '.svg') -PathType Leaf)
        if (-not $iconExists) {
            throw "Missing Stream Deck category icon: $($manifest.CategoryIcon)$suffix.(png|svg)"
        }
    }
}

& $cargoPath build --locked --release --bin audio-switch-stream-deck --target-dir $buildTarget
if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

$legacyHost = Join-Path $plugin 'audio-switch.exe'
if (Test-Path -LiteralPath $legacyHost) {
    Remove-Item -LiteralPath $legacyHost -Force
}
Copy-Item -LiteralPath (Join-Path $buildTarget 'release\audio-switch-stream-deck.exe') -Destination (Join-Path $plugin 'audio-switch-stream-deck.exe') -Force
& (Join-Path $PSScriptRoot 'render-plugin-icon.ps1')
streamdeck validate $plugin --no-update-check
if ($LASTEXITCODE -ne 0) { throw 'Stream Deck validation failed.' }

New-Item -ItemType Directory -Path $distribution -Force | Out-Null
streamdeck pack $plugin --output $distribution --version $Version --force --no-update-check
if ($LASTEXITCODE -ne 0) { throw 'Stream Deck packaging failed.' }
