# Installs the witness binary on Windows from GitHub Releases, checking its
# SHA-256.
#
#   irm https://raw.githubusercontent.com/radical-studios/witness/main/install.ps1 | iex
#
# Environment: WITNESS_VERSION (default: latest), WITNESS_INSTALL_DIR
# (default: %LOCALAPPDATA%\witness\bin).
$ErrorActionPreference = 'Stop'

$repo = 'radical-studios/witness'
$version = if ($env:WITNESS_VERSION) { $env:WITNESS_VERSION } else { 'latest' }
$dest = if ($env:WITNESS_INSTALL_DIR) { $env:WITNESS_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'witness\bin' }
$asset = 'witness-x86_64-pc-windows-msvc.zip'
$base = if ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("witness-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Invoke-WebRequest -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset) -UseBasicParsing
    Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS') -UseBasicParsing
    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match " $([regex]::Escape($asset))$" }
    $expected = ($line -split ' ')[0]
    $actual = (Get-FileHash -Algorithm SHA256 (Join-Path $tmp $asset)).Hash.ToLower()
    if (-not $expected -or $expected -ne $actual) { throw "checksum mismatch for $asset; not installing" }
    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    Copy-Item (Join-Path $tmp 'witness.exe') (Join-Path $dest 'witness.exe') -Force
    Write-Output "witness: installed to $dest\witness.exe"
    if (($env:PATH -split ';') -notcontains $dest) { Write-Output "witness: add $dest to your PATH" }
} finally {
    Remove-Item -Recurse -Force $tmp
}
