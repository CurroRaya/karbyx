#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de forge
#
# Empaqueta forge como app de macOS: target/release/Forge.app
#   scripts/bundle.sh            # compila y crea la app
#   scripts/bundle.sh --install  # y la copia a ~/Applications
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p forge-app

# Icono: se genera una vez desde scripts/icon.swift.
ICNS=assets/forge.icns
if [[ ! -f "$ICNS" ]]; then
    tmp=$(mktemp -d)
    set=$tmp/forge.iconset
    mkdir -p "$set"
    swift scripts/icon.swift "$tmp/icon.png" 1024
    for s in 16 32 128 256 512; do
        sips -z $s $s "$tmp/icon.png" --out "$set/icon_${s}x${s}.png" >/dev/null
        sips -z $((s * 2)) $((s * 2)) "$tmp/icon.png" --out "$set/icon_${s}x${s}@2x.png" >/dev/null
    done
    mkdir -p assets
    iconutil -c icns "$set" -o "$ICNS"
    rm -r "$tmp"
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP=target/release/Forge.app
rm -rf target/release/Forge.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/forge-app "$APP/Contents/MacOS/forge-app"
cp "$ICNS" "$APP/Contents/Resources/forge.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>forge</string>
    <key>CFBundleDisplayName</key><string>forge</string>
    <key>CFBundleIdentifier</key><string>io.github.curroraya.forge</string>
    <key>CFBundleExecutable</key><string>forge-app</string>
    <key>CFBundleIconFile</key><string>forge</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleDevelopmentRegion</key><string>es</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>NSHumanReadableCopyright</key><string>© 2026 Francisco José Raya García (Curro) y los colaboradores de forge · GPL-3.0-or-later</string>
</dict>
</plist>
PLIST

# Firma local (ad hoc): basta para abrirla en este Mac.
codesign --force --sign - "$APP" >/dev/null 2>&1
echo "Creada $APP"

if [[ "${1:-}" == "--install" ]]; then
    mkdir -p "$HOME/Applications"
    rm -rf "$HOME/Applications/Forge.app"
    cp -R "$APP" "$HOME/Applications/Forge.app"
    echo "Instalada en ~/Applications/Forge.app"
fi
