#!/bin/sh
# Builds PKG Master for Linux and packages it two ways, in app/dist/:
#
#   PKG-Master-v<version>-linux-x64.tar.gz
#     PKG-Master-v<version>/
#       pkgmaster-gui
#       pkgmaster-cli
#       README.txt
#       shadpkg/            (empty: users drop their own shadPKG here)
#
#   PKG-Master-v<version>-x86_64.AppImage   (one file, runs on most distros)
#
# Run from anywhere: sh app/package-linux.sh. Needs appimagetool on PATH for
# the AppImage (the GitHub workflow downloads it).
set -eu

app=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$app/Cargo.toml" | head -n 1)
name="PKG-Master-v$version"
dist="$app/dist"
target="${CARGO_TARGET_DIR:-$app/target}"

(cd "$app" && cargo build --release)

rm -rf "$dist/$name" "$dist/AppDir"
mkdir -p "$dist/$name/shadpkg"
cp "$target/release/pkgmaster-gui" "$target/release/pkgmaster-cli" "$dist/$name/"
cat > "$dist/$name/README.txt" <<EOF
PKG Master v$version for Linux
=================================

Run ./pkgmaster-gui. Nothing to install.

Drop a file on the window:
  eboot.bin            -> Convert to ELF
  PS5 game folder      -> Run in AnyPS5, then Launch
  PS4 .pkg             -> Run shadPKG (PS4) to decrypt and extract it
  fake / debug .pkg    -> passcode (often 32 zeros), then convert or extract
  PS3 folder or .iso   -> Run in RPCS3

Emulators aren't included. The app downloads the official release when
you ask. shadPKG: get it from https://github.com/seregonwar/ShadPKG/releases
and put it in the shadpkg folder.

Command line: ./pkgmaster-cli convert eboot.bin eboot.elf

Uninstall with Menu > Uninstall PKG Master. Deleting the folder by hand
leaves settings (~/.config/pkg-master) and downloads behind.

Full guide: https://github.com/Guernica4u/PKG-Master
EOF
tar -C "$dist" -czf "$dist/$name-linux-x64.tar.gz" "$name"
echo "Wrote $dist/$name-linux-x64.tar.gz"

if command -v appimagetool >/dev/null 2>&1; then
    appdir="$dist/AppDir"
    mkdir -p "$appdir/usr/bin"
    cp "$target/release/pkgmaster-gui" "$target/release/pkgmaster-cli" "$appdir/usr/bin/"
    cp "$app/assets/icon.png" "$appdir/pkg-master.png"
    cat > "$appdir/pkg-master.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=PKG Master
Comment=Open, convert and run your own PS4, PS5 and PS3 game dumps
Exec=pkgmaster-gui %f
Icon=pkg-master
Categories=Utility;
Terminal=false
EOF
    cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/pkgmaster-gui" "$@"
EOF
    chmod +x "$appdir/AppRun"
    ARCH=x86_64 appimagetool "$appdir" "$dist/$name-x86_64.AppImage"
    rm -rf "$appdir"
    echo "Wrote $dist/$name-x86_64.AppImage"
else
    echo "appimagetool not found; skipped the AppImage"
fi
