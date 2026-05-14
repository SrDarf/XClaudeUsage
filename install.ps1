# XClaudeUsage installer (Windows PowerShell).
#
# Downloads the latest pre-built xclaudeusage.exe for this host's architecture
# from GitHub Releases, drops it in $HOME\.claude\bin, and execs
# `xclaudeusage install` for interactive settings.json + Turso config.
#
#   irm https://raw.githubusercontent.com/SrDarf/XClaudeUsage/HighPerformanceXClaudeUsage/install.ps1 | iex
#
# Set $env:XCLAUDEUSAGE_VERSION to pin a specific tag (default: latest).

$ErrorActionPreference = 'Stop'

$Repo = 'SrDarf/XClaudeUsage'
$BinDir = Join-Path $HOME '.claude\bin'
$BinPath = Join-Path $BinDir 'xclaudeusage.exe'
$Version = if ($env:XCLAUDEUSAGE_VERSION) { $env:XCLAUDEUSAGE_VERSION } else { 'latest' }

function Info($msg) { Write-Host "[install] $msg" }
function Fail($msg) { Write-Error "[install] $msg"; exit 1 }

$arch = (Get-CimInstance Win32_Processor).Architecture
switch ($arch) {
  9  { $archPart = 'x86_64'  }
  12 { $archPart = 'aarch64' }
  default { Fail "unsupported architecture: $arch" }
}

$target = "$archPart-pc-windows-msvc"
$archive = "xclaudeusage-$target.zip"
$base = if ($Version -eq 'latest') {
  "https://github.com/$Repo/releases/latest/download"
} else {
  "https://github.com/$Repo/releases/download/$Version"
}

$tmp = New-Item -ItemType Directory -Path ([IO.Path]::Combine([IO.Path]::GetTempPath(), [Guid]::NewGuid()))
try {
  Info "downloading $archive from $base"
  Invoke-WebRequest -Uri "$base/$archive" -OutFile (Join-Path $tmp $archive) -UseBasicParsing

  # Verify SHA-256 against published manifest if available.
  try {
    Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS') -UseBasicParsing -ErrorAction Stop
    $expected = (Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match "  $archive`$" }) -split '\s+' | Select-Object -First 1
    if ($expected) {
      $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $archive)).Hash.ToLower()
      if ($actual -ne $expected.ToLower()) {
        Fail "checksum mismatch for $archive (expected $expected, got $actual)"
      }
      Info 'checksum verified'
    }
  } catch {
    # Manifest absent; skip verification.
  }

  Info 'extracting'
  Expand-Archive -Path (Join-Path $tmp $archive) -DestinationPath $tmp -Force
  $extracted = Join-Path $tmp 'xclaudeusage.exe'
  if (-not (Test-Path $extracted)) {
    Fail "archive did not contain 'xclaudeusage.exe'"
  }

  New-Item -ItemType Directory -Force -Path $BinDir | Out-Null
  Move-Item -Force -Path $extracted -Destination $BinPath
  Info "installed $BinPath"

  & $BinPath install
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
