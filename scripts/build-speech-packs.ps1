# Builds the downloadable speech packs listed in
# crates/speech-packs/speech-packs.manifest and prints the manifest values
# (size + sha256) to paste in after uploading them to the `speech-packs-v1`
# GitHub release.
#
#   nemo-speech-cuda-win-x64.zip     Parakeet's CUDA runtime (from runtime/nemo-speech, Git LFS)
#   phonon-cpu-runtime-win-x64.zip   Embedded Python + CPU torch + fermion-research
#   phonon-2-model.zip               The unpacked Phonon-2 profile from Hugging Face
#
# Run on Windows x64 with Python 3.12 on PATH (CI: windows-2022 + setup-python).
param(
    [ValidateSet('all', 'cuda', 'phonon')]
    [string]$Pack = 'all',
    [string]$OutDir = (Join-Path (Split-Path -Parent $PSScriptRoot) 'dist/speech-packs'),
    # Pins for the Phonon runtime. Bump deliberately and re-test.
    [string]$PythonVersion = '3.12.10',
    [string]$FermionVersion = '0.2.7'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path $OutDir 'work'
New-Item -ItemType Directory -Force -Path $OutDir, $work | Out-Null

function New-PackZip([string]$Source, [string]$Zip) {
    if (Test-Path $Zip) { Remove-Item $Zip -Force }
    # Optimal compression; entries are relative to $Source.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [IO.Compression.ZipFile]::CreateFromDirectory($Source, $Zip, [IO.Compression.CompressionLevel]::Optimal, $false)
}

function Write-ManifestValues([string]$Id, [string]$Zip) {
    $item = Get-Item $Zip
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $Zip).Hash.ToLowerInvariant()
    $line = "[$Id] $($item.Name)`n  sha256=$hash`n  size=$($item.Length)"
    Write-Host $line
    Add-Content -Path (Join-Path $OutDir 'manifest-values.txt') -Value $line
}

function Build-Cuda {
    $bin = Join-Path $root 'runtime/nemo-speech/bin'
    $exe = Join-Path $bin 'nemo-speech.exe'
    if ((Get-Item $exe).Length -lt 100KB) {
        throw "runtime/nemo-speech/bin holds Git LFS pointers. Run: git lfs pull --include `"runtime/nemo-speech/**`""
    }
    $stage = Join-Path $work 'nemo-speech-cuda'
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item $bin (Join-Path $stage 'bin') -Recurse
    Copy-Item (Join-Path $root 'runtime/nemo-speech/share/licenses') (Join-Path $stage 'licenses') -Recurse
    $zip = Join-Path $OutDir 'nemo-speech-cuda-win-x64.zip'
    New-PackZip $stage $zip
    Write-ManifestValues 'nemo-speech-cuda' $zip
}

function Build-PhononRuntime {
    $stage = Join-Path $work 'phonon-cpu'
    $python = Join-Path $stage 'python'
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $python | Out-Null

    $embed = Join-Path $work "python-$PythonVersion-embed-amd64.zip"
    if (-not (Test-Path $embed)) {
        Invoke-WebRequest "https://www.python.org/ftp/python/$PythonVersion/python-$PythonVersion-embed-amd64.zip" -OutFile $embed
    }
    Expand-Archive $embed -DestinationPath $python
    # The embeddable distribution ignores site-packages until its ._pth says so.
    $pth = Get-ChildItem $python -Filter 'python3*._pth' | Select-Object -First 1
    $tag = $pth.BaseName
    Set-Content -Path $pth.FullName -Encoding ascii -Value @("$tag.zip", '.', 'Lib\site-packages', 'import site')

    $site = Join-Path $python 'Lib/site-packages'
    $major, $minor = $PythonVersion.Split('.')[0..1]
    $hostVersion = (& python -c "import sys; print(f'{sys.version_info[0]}.{sys.version_info[1]}')").Trim()
    if ($hostVersion -ne "$major.$minor") {
        throw "Host Python is $hostVersion; use Python $major.$minor so wheels match the embedded runtime."
    }
    # CPU-only torch keeps the pack small; everything else comes from PyPI.
    & python -m pip install --disable-pip-version-check --no-warn-script-location --only-binary=:all: `
        --target $site --index-url https://download.pytorch.org/whl/cpu --extra-index-url https://pypi.org/simple `
        torch "fermion-research==$FermionVersion" safetensors soundfile scipy zstandard | Out-Host
    if ($LASTEXITCODE -ne 0) { throw 'pip install failed' }
    & python -m pip freeze --path $site | Set-Content -Encoding utf8 (Join-Path $stage 'requirements.lock.txt')

    # torch needs the MSVC runtime, which a clean Windows install lacks. Ship
    # it app-local beside python.exe (as runtime/nemo-speech/bin already does)
    # instead of requiring vc_redist.x64.exe.
    foreach ($dll in 'msvcp140.dll', 'msvcp140_1.dll', 'msvcp140_2.dll', 'vcruntime140.dll', 'vcruntime140_1.dll', 'concrt140.dll', 'vcomp140.dll') {
        $source = Join-Path $env:WINDIR "System32/$dll"
        if (-not (Test-Path (Join-Path $python $dll)) -and (Test-Path $source)) { Copy-Item $source $python }
    }

    # Trim what a CPU inference runtime never loads.
    Get-ChildItem $site -Recurse -Directory -Include '__pycache__', 'tests', 'test' -ErrorAction SilentlyContinue |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    foreach ($path in 'torch/include', 'torch/share', 'bin') {
        $target = Join-Path $site $path
        if (Test-Path $target) { Remove-Item $target -Recurse -Force }
    }
    Get-ChildItem (Join-Path $site 'torch/lib') -Filter '*.lib' -ErrorAction SilentlyContinue | Remove-Item -Force

    $zip = Join-Path $OutDir 'phonon-cpu-runtime-win-x64.zip'
    New-PackZip $stage $zip
    Write-ManifestValues 'phonon-runtime' $zip
    return (Join-Path $python 'python.exe')
}

function Build-PhononModel([string]$PythonExe) {
    $cache = Join-Path $work 'fermion-cache'
    $env:FERMION_CACHE_DIR = $cache
    $env:FERMION_DEVICE = 'cpu'
    $script = "from fermion.transcribe import _resolve; from fermion._speech import fetch; " +
        "repo, key, pin, _ = _resolve('phonon-2'); print(fetch.ensure(repo, key, pin, quiet=True))"
    $modelDir = (& $PythonExe -I -c $script | Select-Object -Last 1).Trim()
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $modelDir 'config.json'))) {
        throw "Could not fetch Phonon-2 (got '$modelDir')"
    }
    $zip = Join-Path $OutDir 'phonon-2-model.zip'
    New-PackZip $modelDir $zip
    Write-ManifestValues 'phonon-model' $zip
    Test-PhononServer $PythonExe $modelDir
}

# Start the server the way Pronto does and transcribe one second of silence.
function Test-PhononServer([string]$PythonExe, [string]$ModelDir) {
    $port = 18431
    $env:HF_HUB_OFFLINE = '1'
    $server = Start-Process -FilePath $PythonExe -PassThru -NoNewWindow `
        -ArgumentList @('-I', '-B', '-X', 'utf8', '-m', 'fermion.cli', 'serve', "`"$ModelDir`"", '--host', '127.0.0.1', '--port', $port)
    try {
        $deadline = (Get-Date).AddMinutes(4)
        do {
            Start-Sleep -Seconds 2
            try { $ready = (Invoke-WebRequest "http://127.0.0.1:$port/health" -UseBasicParsing).StatusCode -eq 200 } catch { $ready = $false }
            if ($server.HasExited) { throw "Phonon server exited with $($server.ExitCode)" }
        } until ($ready -or (Get-Date) -gt $deadline)
        if (-not $ready) { throw 'Phonon server did not become healthy' }
        $wav = Join-Path $work 'silence.wav'
        $samples = 16000
        $bytes = New-Object byte[] (44 + $samples * 2)
        [Text.Encoding]::ASCII.GetBytes('RIFF').CopyTo($bytes, 0)
        [BitConverter]::GetBytes([int](36 + $samples * 2)).CopyTo($bytes, 4)
        [Text.Encoding]::ASCII.GetBytes('WAVEfmt ').CopyTo($bytes, 8)
        [BitConverter]::GetBytes([int]16).CopyTo($bytes, 16)
        [BitConverter]::GetBytes([int16]1).CopyTo($bytes, 20)
        [BitConverter]::GetBytes([int16]1).CopyTo($bytes, 22)
        [BitConverter]::GetBytes([int]16000).CopyTo($bytes, 24)
        [BitConverter]::GetBytes([int]32000).CopyTo($bytes, 28)
        [BitConverter]::GetBytes([int16]2).CopyTo($bytes, 32)
        [BitConverter]::GetBytes([int16]16).CopyTo($bytes, 34)
        [Text.Encoding]::ASCII.GetBytes('data').CopyTo($bytes, 36)
        [BitConverter]::GetBytes([int]($samples * 2)).CopyTo($bytes, 40)
        [IO.File]::WriteAllBytes($wav, $bytes)
        & curl.exe -sS --fail -F "file=@$wav;type=audio/wav" -F 'response_format=json' "http://127.0.0.1:$port/v1/audio/transcriptions"
        if ($LASTEXITCODE -ne 0) { throw 'Phonon transcription smoke test failed' }
        Write-Host "`nPhonon smoke test passed."
    } finally {
        if (-not $server.HasExited) { Stop-Process -Id $server.Id -Force }
    }
}

Remove-Item (Join-Path $OutDir 'manifest-values.txt') -ErrorAction SilentlyContinue
if ($Pack -in 'all', 'cuda') { Build-Cuda }
if ($Pack -in 'all', 'phonon') {
    $pythonExe = Build-PhononRuntime
    Build-PhononModel $pythonExe
}
Write-Host "`nUpload the zips in $OutDir to the speech-packs-v1 release, then copy the values above into crates/speech-packs/speech-packs.manifest."
