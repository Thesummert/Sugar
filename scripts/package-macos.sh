#!/bin/sh
set -eu
[ "$(uname -s)" = Darwin ] || { echo "macOS required" >&2; exit 1; }
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
npm ci --no-audit --no-fund
npm run build
cargo build --release --locked --features custom-protocol
app="$project_dir/target/macos/Sugar Archive.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/sugar-archive "$app/Contents/MacOS/sugar-archive"
ln -sf sugar-archive "$app/Contents/MacOS/sugar-archive-pudding"
cp macos/Info.plist "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"
echo "Created: $app"
