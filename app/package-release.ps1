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

Drop a game or file on the window. The README says what each kind does.

Windows may warn you because the app isn't signed. It's a false alarm:
click More info, then Run anyway.

Only using it for PS5 games? Try PS5toPC instead:
https://github.com/Guernica4u/PS5toPC

To remove it, use Menu > Uninstall. Deleting the folder leaves your
settings and downloads behind.

Full guide: https://github.com/Guernica4u/PKG-Master
"@ | Set-Content -Encoding utf8 (Join-Path $stage "README.txt")

# ZipFile keeps empty folders (Compress-Archive drops them).
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory($stage, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Write-Host "Wrote $zip"
Write-Host "SHA-256 $hash"
