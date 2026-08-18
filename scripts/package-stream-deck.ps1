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

if ([string]::IsNullOrWhiteSpace($Version)) {
    $metadata = & $cargoPath metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the Cargo workspace version.' }

    $application = $metadata.packages | Where-Object name -EQ 'audio-switch' | Select-Object -First 1
    if ($null -eq $application) { throw 'Could not find the audio-switch Cargo package.' }
    $Version = "$($application.version).0"
} elseif ($Version -match '^\d+\.\d+\.\d+$') {
    $Version = "$Version.0"
}

& $cargoPath build --locked --release --workspace
if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

Copy-Item -LiteralPath (Join-Path $repository 'target\release\audio-switch.exe') -Destination (Join-Path $plugin 'audio-switch.exe') -Force
& (Join-Path $PSScriptRoot 'render-plugin-icon.ps1')
streamdeck validate $plugin --no-update-check
if ($LASTEXITCODE -ne 0) { throw 'Stream Deck validation failed.' }

New-Item -ItemType Directory -Path $distribution -Force | Out-Null
streamdeck pack $plugin --output $distribution --version $Version --force --no-update-check
if ($LASTEXITCODE -ne 0) { throw 'Stream Deck packaging failed.' }
