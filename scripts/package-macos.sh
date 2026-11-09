#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
    echo "此脚本需要在 macOS 上运行。" >&2
    exit 1
fi

# Use the same Qt installation to compile and bundle the application.
if [ -z "${QMAKE:-}" ]; then
    if command -v qmake6 >/dev/null 2>&1; then
        QMAKE=$(command -v qmake6)
    elif command -v qmake >/dev/null 2>&1; then
        QMAKE=$(command -v qmake)
    elif command -v brew >/dev/null 2>&1; then
        QMAKE="$(brew --prefix qt)/bin/qmake"
    else
        echo "需要 Qt 开发环境（例如 brew install qt），或通过 QMAKE 指定 qmake 路径。" >&2
        exit 1
    fi
fi
export QMAKE
qt_bin_dir=$("$QMAKE" -query QT_INSTALL_BINS)
if [ ! -x "$qt_bin_dir/macdeployqt" ]; then
    echo "找不到 macdeployqt：$qt_bin_dir/macdeployqt" >&2
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
# Qt builds using dylibs can reference @rpath without the Rust linker adding
# an rpath. Point dyld at the libraries placed in the bundle by macdeployqt.
if ! otool -l "$app/Contents/MacOS/sugar-archive" | grep -Fq '@executable_path/../Frameworks'; then
    install_name_tool -add_rpath '@executable_path/../Frameworks' "$app/Contents/MacOS/sugar-archive"
fi
"$qt_bin_dir/macdeployqt" "$app" -always-overwrite
codesign --force --deep --sign - "$app"
echo "已生成：$app"
