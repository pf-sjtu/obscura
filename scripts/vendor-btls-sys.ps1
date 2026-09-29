# Restore crates/vendor/btls-sys-0.5.6 from crates.io + local aarch64 patch.
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$ver = "0.5.6"
$crate = "btls-sys-$ver"
$tmp = Join-Path $root "tmp"
$dest = Join-Path $root "crates\vendor\$crate"
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
if (Test-Path $dest) { Write-Host "$dest already exists; skipping."; exit 0 }
$pkg = Join-Path $tmp "$crate.crate"
Invoke-WebRequest -Uri "https://crates.io/api/v1/crates/btls-sys/$ver/download" -OutFile $pkg -UseBasicParsing
Push-Location $tmp
try {
    tar -xzf "$crate.crate"
    Move-Item $crate $dest
} finally { Pop-Location }
$patch = Join-Path $root "vendor-patches\$crate-aarch64-windows.patch"
Push-Location $dest
try { git apply --whitespace=nowarn $patch } catch { Write-Warning "git apply failed: $_" }
Pop-Location
Write-Host "Vendored $crate -> $dest"
