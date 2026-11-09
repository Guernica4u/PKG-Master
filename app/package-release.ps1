# Builds PKG Master and packages the Windows release zip:
#
#   PKG-Master-v<version>/
#     pkgmaster-gui.exe
#     pkgmaster-cli.exe
#     README.txt
#     shadpkg/            (empty: users drop their own shadPKG.exe here)
#
# Run from anywhere:  powershell -File app\package-release.ps1
# The zip and its SHA-256 land in app\dist\.

$ErrorActionPreference = "Stop"
$app = $PSScriptRoot
$version = (Select-String -Path "$app\Cargo.toml" -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$name = "PKG-Master-v$version"
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
# CARGO_TARGET_DIR builds elsewhere (useful while target\release\pkgmaster-gui.exe is running).
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$app\target" }
Copy-Item "$target\release\pkgmaster-gui.exe", "$target\release\pkgmaster-cli.exe" $stage

@"
PKG Master v$version
=================

Run pkgmaster-gui.exe. Nothing to install.

Windows warns because the app isn't code-signed. Blue box: More info, then
Run anyway. Defender blocked it: Windows Security > Protection history >
Allow.

Drop a file on the window:
  eboot.bin            -> Convert to ELF
  PS5 game folder      -> Run in AnyPS5, then Launch
  PS4 .pkg             -> Run shadPKG (PS4) to decrypt and extract it
  fake / debug .pkg    -> passcode (often 32 zeros), then convert or extract
  PS3 folder or .iso   -> Run in RPCS3

Emulators aren't included. The app downloads the official release when
you ask. shadPKG: get shadPKG.exe from
https://github.com/seregonwar/ShadPKG/releases and put it in the shadpkg
folder.

Command line: drop an eboot.bin on pkgmaster-cli.exe, or run
  pkgmaster-cli convert eboot.bin eboot.elf

Uninstall with Menu > Uninstall PKG Master. Deleting the folder by hand
leaves settings and downloads behind.

Full guide: https://github.com/Guernica4u/PKG-Master
"@ | Set-Content -Encoding utf8 (Join-Path $stage "README.txt")

# ZipFile keeps empty folders (Compress-Archive drops them).
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory($stage, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Write-Host "Wrote $zip"
Write-Host "SHA-256 $hash"
