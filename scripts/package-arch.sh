#!/bin/sh
set -eu

if [ "$(uname -s)" != "Linux" ] || ! command -v makepkg >/dev/null 2>&1; then
    echo "此脚本需要在安装了 makepkg 的 Arch Linux 环境中运行。" >&2
    exit 1
fi
if [ "$(id -u)" -eq 0 ]; then
    echo "请使用普通用户运行此脚本；makepkg 不允许以 root 身份构建。" >&2
    exit 1
fi

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
package_dir="$project_dir/target/arch"
version=$(awk '
    /^\[package\]$/ { package = 1; next }
    /^\[/ { package = 0 }
    package && /^version *= *"/ { split($0, fields, "\""); print fields[2]; exit }
' "$project_dir/Cargo.toml")
case "$version" in
    ''|*[!0-9A-Za-z.+]*)
        echo "Cargo.toml 中的版本号不适用于 Arch Linux 软件包：$version" >&2
        exit 1
        ;;
esac

mkdir -p "$package_dir"
source_name="sugar-archive-$version"
archive="$package_dir/$source_name.tar.gz"
# Include the working tree so packaging also works before changes are committed.
tar -czf "$archive" --transform "s,^,$source_name/," -C "$project_dir" \
    Cargo.toml Cargo.lock build.rs src ui linux README.md LICENSE
checksum=$(sha256sum "$archive")
checksum=${checksum%% *}
sed -e "s/^pkgver=.*/pkgver=$version/" \
    -e "s/^sha256sums=.*/sha256sums=('$checksum')/" \
    "$project_dir/arch/PKGBUILD" > "$package_dir/PKGBUILD"

cd "$package_dir"
makepkg --cleanbuild "$@"
echo "已生成的软件包："
makepkg --packagelist
