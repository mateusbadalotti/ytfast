#!/bin/bash
# Packs a release binary into ytfast.app and signs it.
#
#   packaging/macos/bundle.sh BINARY OUTPUT.app VERSION
#
# CODESIGN_IDENTITY names a signing identity. Without one the bundle gets an
# ad-hoc signature, the least Apple silicon will launch.
set -euo pipefail

binary=$1
bundle=$2
version=$3
source_dir=$(cd "$(dirname "$0")" && pwd)
contents="$bundle/Contents"

rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources"
install -m 755 "$binary" "$contents/MacOS/ytfast"

# CFBundleVersion takes digits and dots only: "0.2.0-beta" files as "0.2.0".
sed -e "s/__VERSION__/$version/g" \
    -e "s/__BUILD__/${version%%-*}/g" \
    -e "s/__EXECUTABLE__/ytfast/g" \
    -e "s/__IDENTIFIER__/com.github.mateusbadalotti.ytfast/g" \
    "$source_dir/Info.plist" > "$contents/Info.plist"

# The icon set is cut from the one 1024 px master on every build.
icons=$(mktemp -d)
trap 'rm -rf "$icons"' EXIT
set_dir="$icons/ytfast.iconset"
mkdir "$set_dir"
for px in 16 32 128 256 512; do
    sips -z "$px" "$px" "$source_dir/icon-1024.png" --out "$set_dir/icon_${px}x${px}.png" >/dev/null
    sips -z $((px * 2)) $((px * 2)) "$source_dir/icon-1024.png" --out "$set_dir/icon_${px}x${px}@2x.png" >/dev/null
done
iconutil --convert icns --output "$contents/Resources/ytfast.icns" "$set_dir"

identity=${CODESIGN_IDENTITY:--}
if [ "$identity" = "-" ]; then
    codesign --force --sign - "$bundle"
else
    codesign --force --timestamp --options runtime --sign "$identity" "$bundle"
fi
codesign --verify --strict "$bundle"
echo "$bundle"
