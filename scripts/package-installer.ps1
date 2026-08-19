param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string] $Version,
    [switch] $SkipBuild
)

$ErrorActionPreference = 'Stop'

$repository = Split-Path $PSScriptRoot -Parent
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -eq $cargo) {
    $cargoPath = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
} else {
    $cargoPath = $cargo.Source
}

$metadata = & $cargoPath metadata --format-version 1 --no-deps | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Could not read Cargo metadata.' }
$application = $metadata.packages | Where-Object name -EQ 'audio-switch' | Select-Object -First 1
if ($null -eq $application) { throw 'Could not find the audio-switch Cargo package.' }
if ($application.version -ne $Version) {
    throw "Installer version $Version does not match Cargo version $($application.version)."
}

if (-not $SkipBuild) {
    & $cargoPath build --locked --release --bin audio-switch
    if ($LASTEXITCODE -ne 0) { throw 'Standalone release build failed.' }
}

$compilerCandidates = @(
    $env:INNO_SETUP_COMPILER,
    (Get-Command ISCC.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe')
) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) }
$compiler = $compilerCandidates | Select-Object -First 1
if ($null -eq $compiler) {
    throw 'Inno Setup 6 compiler (ISCC.exe) was not found.'
}

$sourceExecutable = Join-Path $repository 'target\release\audio-switch.exe'
$installerScript = Join-Path $repository 'installer\audio-switch.iss'
& $compiler "/DAppVersion=$Version" "/DSourceExe=$sourceExecutable" $installerScript
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup compilation failed.' }

$output = Join-Path $repository "dist\audio-switch-$Version-setup-windows-x86_64.exe"
if (-not (Test-Path -LiteralPath $output -PathType Leaf)) {
    throw "Installer was not created at $output."
}
$output
