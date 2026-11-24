# Builds ROM Master and packages the Windows release zip:
#
#   ROM-Master-v<version>/
#     rommaster-gui.exe
#     rommaster-cli.exe
#     README.txt
#     shadpkg/            (empty: users drop their own shadPKG.exe here)
#
# Run from anywhere:  powershell -File app\package-release.ps1
# The zip and its SHA-256 land in app\dist\.

$ErrorActionPreference = "Stop"
$app = $PSScriptRoot
$version = (Select-String -Path "$app\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name = "ROM-Master-v$version"
$dist = Join-Path $app "dist"
$stage = Join-Path $dist $name
$zip = Join-Path $dist "$name-windows-x64.zip"

Push-Location $app
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
if (Test-Path $zip) { Remove-Item $zip -Force }
New-Item -ItemType Directory -Force (Join-Path $stage "shadpkg") | Out-Null
# CARGO_TARGET_DIR builds elsewhere (useful while target\release\rommaster-gui.exe is running).
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$app\target" }
Copy-Item "$target\release\rommaster-gui.exe", "$target\release\rommaster-cli.exe" $stage

@"
ROM Master v$version
=================

Run rommaster-gui.exe. Nothing to install.

Drop a game or file on the window. The README says what each kind does.

Windows may warn you because the app isn't signed. It's a false alarm:
click More info, then Run anyway.

Only using it for PS5 or PS4 games? Try PS5toPC instead:
https://github.com/Guernica4u/PS5toPC

To remove it, use Menu > Uninstall. Deleting the folder leaves your
settings and downloads behind.

Full guide: https://github.com/Guernica4u/ROM-Master
"@ | Set-Content -Encoding utf8 (Join-Path $stage "README.txt")

# ZipFile keeps empty folders (Compress-Archive drops them).
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory($stage, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)

# Installs from before the rename update from a PKG-Master-... zip holding the old
# program names (their updater looks for pkgmaster-gui.exe). Same app inside.
$bridge = Join-Path $dist "PKG-Master-v$version"
$bridgeZip = Join-Path $dist "PKG-Master-v$version-windows-x64.zip"
foreach ($old in $bridge, $bridgeZip) { if (Test-Path $old) { Remove-Item $old -Recurse -Force } }
Copy-Item $stage $bridge -Recurse
Rename-Item (Join-Path $bridge "rommaster-gui.exe") "pkgmaster-gui.exe"
Rename-Item (Join-Path $bridge "rommaster-cli.exe") "pkgmaster-cli.exe"
[System.IO.Compression.ZipFile]::CreateFromDirectory($bridge, $bridgeZip, [System.IO.Compression.CompressionLevel]::Optimal, $true)
Write-Host "Wrote $bridgeZip (for PKG Master's updater)"

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Write-Host "Wrote $zip"
Write-Host "SHA-256 $hash"
