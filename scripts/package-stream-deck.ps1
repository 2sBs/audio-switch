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

& $cargoPath build --release --workspace
if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }

Copy-Item -LiteralPath (Join-Path $repository 'target\release\audio-switch.exe') -Destination (Join-Path $plugin 'audio-switch.exe') -Force
& (Join-Path $PSScriptRoot 'render-plugin-icon.ps1')
streamdeck validate $plugin
if ($LASTEXITCODE -ne 0) { throw 'Stream Deck validation failed.' }

New-Item -ItemType Directory -Path $distribution -Force | Out-Null
streamdeck pack $plugin --output $distribution --force
if ($LASTEXITCODE -ne 0) { throw 'Stream Deck packaging failed.' }
