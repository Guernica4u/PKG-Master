#!/bin/sh
# Builds ROM Master for Linux and packages it two ways, in app/dist/:
#
#   ROM-Master-v<version>-linux-x64.tar.gz
#     ROM-Master-v<version>/
#       rommaster-gui
#       rommaster-cli
#       README.txt
#       shadpkg/            (empty: users drop their own shadPKG here)
#
#   ROM-Master-v<version>-x86_64.AppImage   (one file, runs on most distros)
#
# Run from anywhere: sh app/package-linux.sh. Needs appimagetool on PATH for
# the AppImage (the GitHub workflow downloads it).
set -eu

app=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$app/Cargo.toml" | head -n 1)
name="ROM-Master-v$version"
dist="$app/dist"
target="${CARGO_TARGET_DIR:-$app/target}"

(cd "$app" && cargo build --release)

rm -rf "$dist/$name" "$dist/AppDir"
mkdir -p "$dist/$name/shadpkg"
cp "$target/release/rommaster-gui" "$target/release/rommaster-cli" "$dist/$name/"
cat > "$dist/$name/README.txt" <<EOF
ROM Master v$version for Linux
=================================

Run ./rommaster-gui. Nothing to install.

Drop a game or file on the window. The README says what each kind does.

Only using it for PS5 or PS4 games? Try PS5toPC instead:
https://github.com/Guernica4u/PS5toPC

To remove it, use Menu > Uninstall. Deleting the folder leaves your
settings and downloads behind.

Full guide: https://github.com/Guernica4u/ROM-Master
EOF
tar -C "$dist" -czf "$dist/$name-linux-x64.tar.gz" "$name"
echo "Wrote $dist/$name-linux-x64.tar.gz"

# Installs from before the rename update from PKG-Master-... files holding the
# old program names (their updater looks for pkgmaster-gui). Same app inside.
bridge="PKG-Master-v$version"
rm -rf "$dist/$bridge"
cp -r "$dist/$name" "$dist/$bridge"
mv "$dist/$bridge/rommaster-gui" "$dist/$bridge/pkgmaster-gui"
mv "$dist/$bridge/rommaster-cli" "$dist/$bridge/pkgmaster-cli"
tar -C "$dist" -czf "$dist/$bridge-linux-x64.tar.gz" "$bridge"

if command -v appimagetool >/dev/null 2>&1; then
    appdir="$dist/AppDir"
    mkdir -p "$appdir/usr/bin"
    cp "$target/release/rommaster-gui" "$target/release/rommaster-cli" "$appdir/usr/bin/"
    cp "$app/assets/icon.png" "$appdir/rom-master.png"
    cat > "$appdir/rom-master.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=ROM Master
Comment=Open, convert and play your own game dumps, PS1 to PS5, PSP, Vita, GameCube, Wii and Xbox 360
Exec=rommaster-gui %f
Icon=rom-master
Categories=Utility;
Terminal=false
EOF
    cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/rommaster-gui" "$@"
EOF
    chmod +x "$appdir/AppRun"
    ARCH=x86_64 appimagetool "$appdir" "$dist/$name-x86_64.AppImage"
    rm -rf "$appdir"
    echo "Wrote $dist/$name-x86_64.AppImage"
    cp "$dist/$name-x86_64.AppImage" "$dist/$bridge-x86_64.AppImage"
else
    echo "appimagetool not found; skipped the AppImage"
fi
