#!/bin/sh
# Builds PS5toPC for Linux and packages it two ways, in app/dist/:
#
#   PS5toPC-v<version>-linux-x64.tar.gz
#     PS5toPC-v<version>/
#       ps5topc-gui
#       ps5topc-cli
#       README.txt
#       shadpkg/            (empty: users drop their own shadPKG here)
#
#   PS5toPC-v<version>-x86_64.AppImage   (one file, runs on most distros)
#
# Run from anywhere: sh app/package-linux.sh. Needs appimagetool on PATH for
# the AppImage (the GitHub workflow downloads it).
set -eu

app=$(cd "$(dirname "$0")" && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$app/Cargo.toml" | head -n 1)
name="PS5toPC-v$version"
dist="$app/dist"
target="${CARGO_TARGET_DIR:-$app/target}"

(cd "$app" && cargo build --release)

rm -rf "$dist/$name" "$dist/AppDir"
mkdir -p "$dist/$name/shadpkg"
cp "$target/release/ps5topc-gui" "$target/release/ps5topc-cli" "$dist/$name/"
cat > "$dist/$name/README.txt" <<EOF
PS5toPC v$version for Linux
=================================

Run ./ps5topc-gui. Nothing to install.

Drop a PS5 game folder or an eboot.bin on the window. The first time,
pick AnyPS5 or KytyPS5 to run your games. The app downloads them for you.

To remove it, use Menu > Uninstall. Deleting the folder leaves your
settings and downloads behind.

Full guide: https://github.com/Guernica4u/PKG-Master/tree/main/PS5toPC
EOF
tar -C "$dist" -czf "$dist/$name-linux-x64.tar.gz" "$name"
echo "Wrote $dist/$name-linux-x64.tar.gz"

if command -v appimagetool >/dev/null 2>&1; then
    appdir="$dist/AppDir"
    mkdir -p "$appdir/usr/bin"
    cp "$target/release/ps5topc-gui" "$target/release/ps5topc-cli" "$appdir/usr/bin/"
    cp "$app/assets/icon.png" "$appdir/ps5topc.png"
    cat > "$appdir/ps5topc.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=PS5toPC
Comment=Turn your own PS5 game dumps into something your PC runs
Exec=ps5topc-gui %f
Icon=ps5topc
Categories=Utility;
Terminal=false
EOF
    cat > "$appdir/AppRun" <<'EOF'
#!/bin/sh
here=$(dirname "$(readlink -f "$0")")
exec "$here/usr/bin/ps5topc-gui" "$@"
EOF
    chmod +x "$appdir/AppRun"
    ARCH=x86_64 appimagetool "$appdir" "$dist/$name-x86_64.AppImage"
    rm -rf "$appdir"
    echo "Wrote $dist/$name-x86_64.AppImage"
else
    echo "appimagetool not found; skipped the AppImage"
fi
