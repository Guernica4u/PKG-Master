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

If Windows says "Windows protected your PC", click More info, then Run anyway
(the app isn't code-signed).

- Open file... / Open folder...: a fake-signed or debug .pkg, or a decrypted
  game folder you dumped yourself.
- Convert to ELF...: eboot.bin -> standard ELF (the file type AnyPS5 needs).
- Run in AnyPS5...: needs AnyPS5 (https://github.com/boykopovar/AnyPS5/releases).
  The first time, the app offers to download the official release for you.
- Run shadPKG (PS4)...: download shadPKG.exe from its official releases
  (https://github.com/seregonwar/ShadPKG/releases) and drop it in the
  shadpkg folder next to this file.
- History: undo anything the app wrote.
- Library: star games to save them; play them from the Library button.
- Run in RPCS3: plays a PS3 game you dumped, with your own RPCS3 (https://rpcs3.net).
- Updates: when a new version is out, the app offers to update itself.
- Shows the game's title, title ID, version and icon when you open it.

No Sony keys are included; retail packages can't be decrypted.
Full guide: https://github.com/Guernica4u/PKG-Master
"@ | Set-Content -Encoding utf8 (Join-Path $stage "README.txt")

# ZipFile keeps empty folders (Compress-Archive drops them).
Add-Type -AssemblyName System.IO.Compression.FileSystem
[System.IO.Compression.ZipFile]::CreateFromDirectory($stage, $zip, [System.IO.Compression.CompressionLevel]::Optimal, $true)

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Write-Host "Wrote $zip"
Write-Host "SHA-256 $hash"
