#!/bin/bash
set -e

cargo build --release

APP=ginger-code.app
rm -rf $APP
mkdir -p $APP/Contents/MacOS
mkdir -p $APP/Contents/Resources   # NEW — standard location for app icons / other bundle resources

cp target/release/ginger-code $APP/Contents/MacOS/ginger-code
cp target/release/ginger-code-cli $APP/Contents/MacOS/ginger-code-cli
cp Info.plist $APP/Contents/

# ── Icon generation (NEW) ─────────────────────────────────────────────────────
#
# macOS app icons must be .icns, not a flat .png. iconutil builds one from an
# .iconset folder containing the source image pre-scaled to each required size.
# This regenerates the .icns from assets/ginger-code.png every build, so the
# PNG stays the single source of truth — no separately-maintained .icns to
# forget to update when the artwork changes.
ICON_SRC=assets/ginger-code.png
ICONSET=ginger-code.iconset
ICNS_NAME=ginger-code.icns

if [ -f "$ICON_SRC" ]; then
    rm -rf "$ICONSET"
    mkdir "$ICONSET"

    # sips is the built-in macOS image tool — no extra dependency needed.
    # Each size needs both a 1x and a 2x (Retina, "@2x") variant; iconutil
    # expects this exact naming convention.
    sips -z 16 16     "$ICON_SRC" --out "$ICONSET/icon_16x16.png"      > /dev/null
    sips -z 32 32     "$ICON_SRC" --out "$ICONSET/icon_16x16@2x.png"   > /dev/null
    sips -z 32 32     "$ICON_SRC" --out "$ICONSET/icon_32x32.png"      > /dev/null
    sips -z 64 64     "$ICON_SRC" --out "$ICONSET/icon_32x32@2x.png"   > /dev/null
    sips -z 128 128   "$ICON_SRC" --out "$ICONSET/icon_128x128.png"    > /dev/null
    sips -z 256 256   "$ICON_SRC" --out "$ICONSET/icon_128x128@2x.png" > /dev/null
    sips -z 256 256   "$ICON_SRC" --out "$ICONSET/icon_256x256.png"    > /dev/null
    sips -z 512 512   "$ICON_SRC" --out "$ICONSET/icon_256x256@2x.png" > /dev/null
    sips -z 512 512   "$ICON_SRC" --out "$ICONSET/icon_512x512.png"    > /dev/null
    sips -z 1024 1024 "$ICON_SRC" --out "$ICONSET/icon_512x512@2x.png" > /dev/null

    iconutil -c icns "$ICONSET" -o "$ICNS_NAME"
    rm -rf "$ICONSET"

    cp "$ICNS_NAME" "$APP/Contents/Resources/$ICNS_NAME"
    echo "Icon bundled: $ICNS_NAME"
else
    echo "Warning: $ICON_SRC not found — building without an icon"
fi

# Tell Finder to pick up the new icon immediately instead of showing a
# stale cached one — harmless if it's already gone.
touch "$APP"

echo "Built $APP"