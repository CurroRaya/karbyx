#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx
#
# Empaqueta Karbyx como app de macOS: target/release/Karbyx.app
#   scripts/bundle.sh            # compila y crea la app
#   scripts/bundle.sh --install  # y la copia a ~/Applications
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p karbyx-app

# Icono: se genera una vez desde scripts/icon.swift.
ICNS=assets/karbyx.icns
if [[ ! -f "$ICNS" ]]; then
    tmp=$(mktemp -d)
    set=$tmp/karbyx.iconset
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
APP=target/release/Karbyx.app
rm -rf target/release/Karbyx.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/karbyx-app "$APP/Contents/MacOS/karbyx-app"
cp "$ICNS" "$APP/Contents/Resources/karbyx.icns"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Karbyx</string>
    <key>CFBundleDisplayName</key><string>Karbyx</string>
    <key>CFBundleIdentifier</key><string>io.github.curroraya.karbyx</string>
    <key>CFBundleExecutable</key><string>karbyx-app</string>
    <key>CFBundleIconFile</key><string>karbyx</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleDevelopmentRegion</key><string>es</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSLocalNetworkUsageDescription</key><string>Karbyx se conecta al servidor de modelos de tu red.</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>NSHumanReadableCopyright</key><string>© 2026 Francisco José Raya García (Curro) y los colaboradores de Karbyx · GPL-3.0-or-later</string>
</dict>
</plist>
PLIST

# Firma local (ad hoc): basta para abrirla en este Mac.
codesign --force --sign - "$APP" >/dev/null 2>&1
echo "Creada $APP"

if [[ "${1:-}" == "--install" ]]; then
    mkdir -p "$HOME/Applications"
    rm -rf "$HOME/Applications/Karbyx.app"
    cp -R "$APP" "$HOME/Applications/Karbyx.app"
    echo "Instalada en ~/Applications/Karbyx.app"
fi
