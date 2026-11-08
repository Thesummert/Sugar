# Sugar Archive

基于 Rust 和 Slint 的轻量级压缩包管理工具，提供亮色与暗色界面。

## 运行

安装 Rust 工具链及当前平台所需的 Slint 图形依赖，然后执行：

```sh
cargo run --release
```

## macOS 打包

在 macOS 上安装 Rust 工具链及 Xcode 命令行工具后，执行：

```sh
sh scripts/package-macos.sh
```

生成的 `target/macos/Sugar Archive.app` 可拖入“应用程序”文件夹。应用声明了支持的压缩包类型，可在 Finder 的“打开方式”中选择 Sugar Archive，也可通过“显示简介”修改默认打开方式。ZIP 可创建和编辑，其他列出的格式仅支持浏览和解压。由于 Finder 按扩展名识别文件，`.gz`、`.bz2`、`.xz` 也可能出现在“打开方式”中；应用仅支持其中的 TAR 归档。脚本使用本机临时签名，分发给其他 Mac 需要开发者签名和公证。

## Arch Linux 打包

安装构建依赖后，使用普通用户运行打包脚本：

```sh
sudo pacman -S --needed base-devel rust desktop-file-utils
sh scripts/package-arch.sh --syncdeps
```

脚本将当前源码（包括尚未提交的修改）打包到 `target/arch/`，生成带 SHA-256 校验的 `PKGBUILD`，再通过 `makepkg` 构建、运行测试并生成 `.pkg.tar.zst` 软件包。首次构建需要联网下载 Rust 依赖；`--syncdeps` 会通过 pacman 安装缺少的依赖。也可使用 `sh scripts/package-arch.sh --syncdeps --install` 在构建成功后安装。构建所需的 Rust 版本以 Slint 的要求为准，请使用已更新的 Arch Linux 环境。

安装后，应用菜单会将 Sugar Archive 识别为压缩包管理器，文件管理器的“打开方式”可将 ZIP、7z、TAR、TGZ、TBZ/TBZ2 和 TXZ 等受支持的压缩包传给应用。文件关联通过系统已有的 MIME 类型声明，不关联不支持的 RAR 或独立 GZ/BZ2/XZ 文件。pacman 的桌面数据库钩子会在安装、升级和卸载时更新关联。

如需将其设为默认打开程序，可在文件管理器中设置，或执行：

```sh
xdg-mime default com.sugararchive.app.desktop application/zip application/x-7z-compressed application/x-tar application/x-compressed-tar application/x-bzip2-compressed-tar application/x-bzip-compressed-tar application/x-xz-compressed-tar
```

文件选择对话框使用 XDG Desktop Portal，需要安装并运行适合当前桌面的后端，例如 GNOME/Xfce 的 `xdg-desktop-portal-gtk` 或 KDE 的 `xdg-desktop-portal-kde`。

## 功能

- 打开、浏览和解压 ZIP、7z、TAR、TAR.GZ（TGZ）、TAR.BZ2（TBZ/TBZ2）和 TAR.XZ（TXZ）。
- 创建 ZIP 压缩包，向当前目录添加文件；其他格式目前为只读。
- 将文件或文件夹拖入窗口，浏览待压缩内容后点击“压缩”，在应用内设置压缩包名称、保存位置和压缩程度；可继续拖入更多项目，保留文件夹结构和空文件夹。
- 通过侧边栏目录树浏览内容，单击选择文件或目录后，使用顶部“解压所选”按钮单独解压，也可以解压全部；双击文件夹可进入。单独解压时保留所选目录名，省略其上级路径。
- 文件列表按文件夹、文档、图片、音视频、代码和压缩包显示不同类型图标。
- 双击文件可将其解压到临时目录并用系统默认程序打开；临时文件在应用退出时清理，对临时文件的修改不会写回压缩包。
- 在应用内设置解压位置，并可选择创建以压缩包文件名（不含扩展名）命名的额外文件夹。
- 解压时显示进度和预计剩余时间；估算时间会随解压速度变化。
- 在菜单栏的“设置 → 外观”窗口中切换亮色和暗色主题。
- 打开、添加和解压在后台执行，文件列表按需渲染。

解压前会检查路径和目标文件，避免覆盖已有文件。暂不支持 RAR、独立的 GZ/BZ2/XZ 文件、加密压缩包、链接或特殊文件的解压、向已有压缩包添加文件夹及删除条目。拖入打包时不支持符号链接或特殊文件。TAR 文件名需要是 UTF-8。TAR 类格式浏览时需要顺序读取压缩数据，因此大压缩包可能需要较长时间；解压失败时可能留下已解压的文件。主题设置不会跨次运行保存。
