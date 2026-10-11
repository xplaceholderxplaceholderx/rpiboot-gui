#!/bin/sh
# Builds dist/rpiboot-gui.app and dist/rpiboot-gui-<version>-<arch>.dmg.
# The app is ad-hoc signed, not notarised: on another Mac, right-click > Open
# the first time.
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
ARCH=$(uname -m)
APP=dist/rpiboot-gui.app
DMG=dist/rpiboot-gui-$VERSION-$ARCH.dmg

cargo build --release
rm -rf dist
mkdir -p "$APP/Contents/MacOS" dist/dmg
cp target/release/rpiboot-gui "$APP/Contents/MacOS/"
sed "s/@VERSION@/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
codesign --force --sign - "$APP"

cp -R "$APP" dist/dmg/
ln -s /Applications dist/dmg/Applications
hdiutil create -quiet -volname "rpiboot-gui" -srcfolder dist/dmg -ov -format UDZO "$DMG"
rm -rf dist/dmg
echo "$DMG"
