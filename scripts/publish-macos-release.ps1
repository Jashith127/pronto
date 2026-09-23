param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')]
    [string]$Repo,
    [string]$Tag = 'v0.8.2-macos'
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$asset = Join-Path $root 'release-assets/Pronto_0.8.2_aarch64.dmg'
$checksumFile = Join-Path $root 'release-assets/Pronto_0.8.2_aarch64.dmg.sha256'
$notes = Join-Path $root 'docs/macos-release-notes.md'

if (-not (Test-Path -LiteralPath $asset -PathType Leaf)) {
    throw "Missing $asset. Copy the entire workspace folder from the Mac, including release-assets."
}
if (-not (Test-Path -LiteralPath $checksumFile -PathType Leaf)) {
    throw "Missing $checksumFile."
}
$expected = ((Get-Content -LiteralPath $checksumFile -Raw).Trim() -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash -LiteralPath $asset -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) {
    throw "DMG checksum mismatch. Expected $expected but found $actual. Copy the DMG again."
}

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw 'Install GitHub CLI (gh), then run gh auth login on this Windows laptop.'
}
$repoName = gh repo view $Repo --json nameWithOwner --jq .nameWithOwner
if ($LASTEXITCODE -ne 0 -or $repoName.Trim() -ne $Repo) {
    throw "GitHub CLI cannot access $Repo. Check the account with gh auth status and create the repository first."
}

gh release view $Tag --repo $Repo --json tagName --jq .tagName 2>$null | Out-Null
if ($LASTEXITCODE -eq 0) {
    throw "Release $Tag already exists in $Repo. Open it on GitHub to add or replace assets manually."
}

gh release create $Tag $asset --repo $Repo --draft --title "Pronto for Mac 0.8.2 (Apple Silicon)" --notes-file $notes
if ($LASTEXITCODE -ne 0) {
    throw 'GitHub did not create the draft release. No public release was published.'
}
Write-Host "Draft release created in $Repo. Review it on GitHub, then publish it when ready."
