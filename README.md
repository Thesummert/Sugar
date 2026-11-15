# Sugar Archive

基于 Rust、Tauri 2 和 Svelte 5 的轻量级压缩包管理器，提供亮色和暗色界面。

## 功能

- 浏览和解压 ZIP、7z、RAR、TAR、TAR.GZ、TAR.BZ2 和 TAR.XZ；创建除 RAR 外的上述格式，并向已有 ZIP 添加文件。
- 将文件或文件夹拖入窗口，浏览待压缩内容，设置格式、名称、保存位置和压缩程度。支持继续拖入更多项目。
- 通过目录树浏览，双击进入文件夹或使用系统默认程序打开文件；解压全部或所选文件，支持额外创建文件夹并显示进度及预计剩余时间。
- 将归档内项目拖到文件管理器进行复制。首次拖动时请按住鼠标等待临时文件准备；提前松开可再次拖动。临时文件保留到应用退出。
- 提供亮色和暗色外观，以及 `sugar-archive-pudding` 命令行接口。

解压时检查路径并避免覆盖已有文件。暂不支持加密归档、解压链接或特殊文件、删除条目、向已有归档添加文件夹，以及单独的 GZ/BZ2/XZ 文件。RAR 依赖系统 libarchive。主题设置不跨运行保存。

## 开发和构建

需要 Rust、Node.js、Tauri 2 对应平台的系统依赖，以及 libarchive。Tauri 的平台依赖请参阅 [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)。Windows 需要 MSVC、WebView2 和 libarchive 的导入库及运行时 DLL；macOS 自带 libarchive；Linux 需要 WebKitGTK 4.1、GTK 3 和 libarchive 开发包。

Windows 构建会依次查找 `LIBARCHIVE_ROOT`、`VCPKG_ROOT/installed/<目标平台 triplet>` 和项目中的 `target/windows-tools/libarchive-static`（仅 x64）。目录应包含 `lib/archive.lib` 或 `lib/libarchive.lib`，以及包含运行时 DLL 和依赖的 `bin` 目录。构建脚本会将 DLL 放在 Cargo 的输出目录中。也可以通过 `LIB` 和 `PATH` 手动配置库和 DLL。更换库时请重启正在运行的开发程序。

```sh
npm ci
npm run tauri dev
```

生产构建使用 `npm run build` 后执行 `cargo build --release --locked --features custom-protocol`，以将前端资源嵌入程序。也可通过 `npm run tauri build` 使用 Tauri 打包器；需要确保 libarchive 能在目标系统加载。前端检查使用 `npm run check`，Rust 测试使用 `cargo test --locked`。

## 平台打包

- Windows：安装 Rust MSVC 工具链、Node.js、Visual Studio C++ x64 构建工具、动态链接的 libarchive 和 Inno Setup 6，双击 `build-windows.bat` 即可构建。脚本自动查找 Visual Studio，并按 `LIBARCHIVE_ROOT`、`VCPKG_ROOT`、项目的 `target/windows-tools/libarchive-static` 顺序查找 libarchive；Inno Setup 可安装到默认位置，也可放在 `target/windows-tools/inno`。首次运行可能需要联网下载依赖。生成的安装程序位于 `target/windows/`，包含 libarchive DLL 和 Pudding 命令；目标机器需要 WebView2 Runtime。也可以使用 `scripts/package-windows.ps1` 的 `-ArchiveRoot` 和 `-Iscc` 参数指定路径，或在自动化中使用 `build-windows.bat --no-pause`。
- macOS：运行 `sh scripts/package-macos.sh`，生成 `target/macos/Sugar Archive.app`。应用采用本地临时签名；分发需要开发者签名和公证。
- Arch Linux：安装 `base-devel rust nodejs npm desktop-file-utils` 后运行 `sh scripts/package-arch.sh --syncdeps`。应用通过桌面文件声明支持的 MIME 类型，文件选择需要适合当前桌面的 XDG Desktop Portal 后端。

Pudding 集成需要安装包含 `sugar-archive-pudding` 的版本。
