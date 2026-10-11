#!/usr/bin/env bash
# Wraps a built familiar-native binary in Familiar.app and signs it ad hoc (no Apple Developer account yet, so the app
# is not notarized: the first launch needs System Settings → Privacy & Security → Open Anyway).
#
#   bundle.sh <familiar-native binary> <version, e.g. 0.2.0 or 0.2.0-alpha.1> <output folder>
#
# Writes <output folder>/Familiar.app. Used by the release workflow (on the universal binary) and CI's smoke test.
set -euo pipefail

bin=$1
version=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)

if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$ ]]; then
  echo "bad version '$version'" >&2
  exit 1
fi
# Bundle versions are numbers only: 0.2.0-alpha.1 -> 0.2.0 (FamiliarVersion keeps the full string).
numeric=${version%%-*}

app="$out/Familiar.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/familiar-native"
chmod 755 "$app/Contents/MacOS/familiar-native"
sed -e "s/@VERSION@/$numeric/g" -e "s/@FULL_VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# The icon: every size iconutil wants, from the 512 px mark.
work=$(mktemp -d)
iconset="$work/Familiar.iconset"
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$here/familiar.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z "$d" "$d" "$here/familiar.png" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Familiar.icns"
rm -rf "$work"

cp "$root/../../LICENSE" "$app/Contents/Resources/LICENSE"
cp "$root/THIRD_PARTY_NOTICES.md" "$app/Contents/Resources/THIRD_PARTY_NOTICES.md"

# Ad hoc signature over the whole bundle (Apple Silicon refuses unsigned code; this also seals Info.plist).
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict --verbose=2 "$app"
echo "built $app ($version)"
