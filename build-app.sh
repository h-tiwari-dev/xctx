#!/bin/sh
# Build xctx.app (release) next to this script. Usage: ./build-app.sh && open xctx.app
set -eu
cd "$(dirname "$0")"
swift build -c release
APP=xctx.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$(swift build -c release --show-bin-path)/xctx" "$APP/Contents/MacOS/xctx"
cat > "$APP/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>xctx</string>
    <key>CFBundleDisplayName</key><string>xctx</string>
    <key>CFBundleIdentifier</key><string>dev.htiwari.xctx</string>
    <key>CFBundleExecutable</key><string>xctx</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSPrincipalClass</key><string>NSApplication</string>
    <key>NSAppTransportSecurity</key><dict><key>NSAllowsArbitraryLoads</key><true/></dict>
</dict>
</plist>
EOF
codesign --force --sign - "$APP" >/dev/null 2>&1 || true
echo "built $(pwd)/$APP"
