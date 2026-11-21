#!/bin/sh
# Builds target/release/Aguin.app from the release binary, assets/Info.plist and assets/icon.png.
# With --universal, the binary runs on Apple silicon and Intel Macs.
set -eu
cd "$(dirname "$0")/.."

if [ "${1:-}" = "--universal" ]; then
	cargo build --release --locked --target aarch64-apple-darwin
	cargo build --release --locked --target x86_64-apple-darwin
	mkdir -p target/universal
	bin=target/universal/aguin
	lipo -create -output "$bin" \
		target/aarch64-apple-darwin/release/aguin \
		target/x86_64-apple-darwin/release/aguin
else
	cargo build --release
	bin=target/release/aguin
fi

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
app=target/release/Aguin.app
iconset=target/release/AppIcon.iconset

rm -rf "$app" "$iconset"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" "$iconset"

# iconutil needs every standard size, each at 1x and 2x.
for size in 16 32 128 256 512; do
	sips -z "$size" "$size" assets/icon.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
	double=$((size * 2))
	sips -z "$double" "$double" assets/icon.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"
rm -rf "$iconset"

cp "$bin" "$app/Contents/MacOS/aguin"
sed "s/@VERSION@/$version/g" assets/Info.plist >"$app/Contents/Info.plist"
plutil -lint -s "$app/Contents/Info.plist"

# An ad-hoc signature, which Apple silicon requires before it runs a bundle.
codesign --force --sign - "$app"

echo "Built $app"
