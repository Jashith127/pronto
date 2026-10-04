# Builds Pronto Setup: compiles the app, packs it into the installer payload,
# and compiles the custom Tauri installer around it.
#
#   dist/Pronto_Setup_<version>_x64.exe
#
# Speech runtimes and models are NOT inside the installer; it downloads the
# packs listed in crates/speech-packs/speech-packs.manifest for the engine the
# user picks. Run on Windows x64 with Rust and Node.js installed.
param(
    [string]$OutDir = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist')
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = (Get-Content (Join-Path $root 'src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json).version
$tauri = @('--yes', '@tauri-apps/cli@2', 'build', '--no-bundle')
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

# 1. The app itself (embedded UI, release profile).
Push-Location (Join-Path $root 'src-tauri')
try {
    & npx @tauri
    if ($LASTEXITCODE -ne 0) { throw 'Pronto build failed' }
} finally { Pop-Location }

# 2. Payload: what used to sit beside pronto.exe in the NSIS install, minus the
#    CUDA runtime, which is now a downloadable pack.
$stage = Join-Path $OutDir 'payload'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path (Join-Path $stage 'resources') | Out-Null
Copy-Item (Join-Path $root 'src-tauri/target/release/pronto.exe') $stage
Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.md') $stage
Copy-Item (Join-Path $root 'src-tauri/resources/design-system.json') (Join-Path $stage 'resources')
$licenses = Join-Path $stage 'runtime/nemo-speech/share'
New-Item -ItemType Directory -Force -Path $licenses | Out-Null
Copy-Item (Join-Path $root 'runtime/nemo-speech/share/licenses') $licenses -Recurse
$payload = Join-Path $OutDir 'pronto-payload.zip'
if (Test-Path $payload) { Remove-Item $payload -Force }
Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory($stage, $payload, [IO.Compression.CompressionLevel]::Optimal, $false)

# 3. The installer, with the payload compiled in.
$env:PRONTO_PAYLOAD_ZIP = $payload
Push-Location (Join-Path $root 'installer')
try {
    & npx @tauri
    if ($LASTEXITCODE -ne 0) { throw 'Pronto Setup build failed' }
} finally {
    Pop-Location
    Remove-Item Env:PRONTO_PAYLOAD_ZIP
}
$setup = Join-Path $OutDir "Pronto_Setup_${version}_x64.exe"
Copy-Item (Join-Path $root 'installer/target/release/pronto-setup.exe') $setup -Force
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $setup).Hash.ToLowerInvariant()
Set-Content -Encoding ascii -Path "$setup.sha256" -Value "$hash  $(Split-Path -Leaf $setup)"
Write-Host "Built $setup ($([math]::Round((Get-Item $setup).Length / 1MB, 1)) MB)"
