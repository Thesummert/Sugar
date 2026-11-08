#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
    echo "此脚本需要在 macOS 上运行。" >&2
    exit 1
fi

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_dir"
app="$project_dir/target/macos/Sugar Archive.app"
cargo build --release --locked
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$project_dir/target/release/sugar-archive" "$app/Contents/MacOS/sugar-archive"
cp "$project_dir/macos/Info.plist" "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
codesign --force --deep --sign - "$app"
echo "已生成：$app"
