# get.ps1 - one-line installer for the Denia client on Windows (prebuilt, signed).
#   Linux/macOS: use get.sh
#
#   irm https://raw.githubusercontent.com/zainokta/denia/main/get.ps1 | iex
#
# Read-first form (recommended):
#   irm https://raw.githubusercontent.com/zainokta/denia/main/get.ps1 -OutFile get.ps1
#   Get-Content get.ps1 ; .\get.ps1
#
# The Windows asset is the Denia client only (server modules are Linux-gated).
#
# Trust chain (fail-closed, same as `denia update` / ADR-029):
#   1. verify SHA256SUMS.minisig over SHA256SUMS with the pinned minisign key;
#   2. verify the downloaded binary's SHA256 against the now-trusted SHA256SUMS.
#
# Env overrides:
#   $env:DENIA_VERSION = 'vX.Y.Z'   install a specific release (default: latest)
#   $env:DENIA_BIN_DIR = 'C:\path'  install dir (default: %LOCALAPPDATA%\Programs\Denia)
#   $env:DENIA_SKIP_MINISIGN = '1'  skip signature check when minisign is absent (NOT recommended)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = 'zainokta/denia'
# Pinned minisign public key (matches key.pub / ADR-029). Verified, not TOFU.
$PubKey = 'RWTjef0vJl3g2lcJz4JSOlDB64pmYBRYNHxmShlHtCbbjcm4aMIj+vkP'
$Asset  = 'denia-x86_64-pc-windows-msvc.exe'

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64') {
  Write-Error "Only x86_64 Windows is published (detected: $arch). No prebuilt binary for this arch."
}

$BinDir = if ($env:DENIA_BIN_DIR) { $env:DENIA_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\Denia' }

# Resolve version (override with $env:DENIA_VERSION).
$Tag = $env:DENIA_VERSION
if (-not $Tag) {
  $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -Headers @{ 'User-Agent' = 'denia-get' }
  $Tag = $rel.tag_name
}
if (-not $Tag) { Write-Error 'Could not resolve latest release tag.' }

$Base = "https://github.com/$Repo/releases/download/$Tag"
$Tmp  = Join-Path ([IO.Path]::GetTempPath()) ("denia-" + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $Tmp -Force | Out-Null
try {
  Write-Host 'Note: the Windows build is the Denia client only (no server; server is Linux-only).'
  Write-Host "Downloading $Asset $Tag..."
  Invoke-WebRequest -Uri "$Base/$Asset"           -OutFile (Join-Path $Tmp 'denia.exe')        -UseBasicParsing
  Invoke-WebRequest -Uri "$Base/SHA256SUMS"        -OutFile (Join-Path $Tmp 'SHA256SUMS')        -UseBasicParsing
  Invoke-WebRequest -Uri "$Base/SHA256SUMS.minisig" -OutFile (Join-Path $Tmp 'SHA256SUMS.minisig') -UseBasicParsing

  # 1) Verify the signature over SHA256SUMS with the pinned key (fail-closed).
  $minisign = Get-Command minisign -ErrorAction SilentlyContinue
  if ($minisign) {
    & $minisign.Source -V -P $PubKey -m (Join-Path $Tmp 'SHA256SUMS') -x (Join-Path $Tmp 'SHA256SUMS.minisig')
    if ($LASTEXITCODE -ne 0) { Write-Error 'minisign verification failed.' }
  } elseif ($env:DENIA_SKIP_MINISIGN -ne '1') {
    Write-Error "minisign not found. Install it (winget install jedisct1.minisign / scoop install minisign) or re-run with `$env:DENIA_SKIP_MINISIGN='1' (NOT recommended)."
  }

  # 2) Verify the binary's checksum against the (now-trusted) SHA256SUMS.
  $expectLine = Get-Content (Join-Path $Tmp 'SHA256SUMS') | Where-Object { $_ -match "\s$([regex]::Escape($Asset))$" } | Select-Object -First 1
  if (-not $expectLine) { Write-Error "No checksum for $Asset in SHA256SUMS." }
  $expect = ($expectLine -split '\s+')[0].ToLower()
  $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $Tmp 'denia.exe')).Hash.ToLower()
  if ($expect -ne $actual) { Write-Error 'Checksum mismatch. Aborting.' }

  # 3) Install.
  New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
  $dest = Join-Path $BinDir 'denia.exe'
  Copy-Item (Join-Path $Tmp 'denia.exe') $dest -Force
  Write-Host "Installed denia $Tag to $dest"

  # Add to the user PATH if missing.
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  if (($userPath -split ';') -notcontains $BinDir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$BinDir", 'User')
    Write-Host "Added $BinDir to your user PATH (restart the shell to pick it up)."
  }
  Write-Host 'Client installed. Run: denia --help'
} finally {
  Remove-Item -Recurse -Force $Tmp -ErrorAction SilentlyContinue
}
