#![cfg_attr(
    all(target_os = "windows", not(feature = "pudding-cli")),
    windows_subsystem = "windows"
)]

use anyhow::{bail, Context, Result};
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

mod rar;

fn size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    for unit in ["KB", "MB", "GB", "TB"] {
        value /= 1024.0;
        if value < 1024.0 || unit == "TB" {
            return format!("{value:.1} {unit}");
        }
    }
    unreachable!()
}

fn file_icon(name: &str, directory: bool) -> &'static str {
    if directory {
        return "folder";
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "svg" | "ico" | "heic" => "image",
        "mp3" | "wav" | "flac" | "aac" | "ogg" | "m4a" => "audio",
        "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => "video",
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "tbz" | "tbz2" | "txz" => {
            "archive"
        }
        "rs" | "c" | "h" | "cpp" | "hpp" | "py" | "js" | "ts" | "jsx" | "tsx" | "html" | "css"
        | "json" | "toml" | "yaml" | "yml" | "xml" | "sh" | "java" | "go" => "code",
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "xls" | "xlsx" | "csv" | "ppt"
        | "pptx" => "document",
        _ => "file",
    }
}

// Some ZIP producers store UTF-8 names without setting the UTF-8 flag.
// Keep the ZIP library's decoded name for byte sequences that are not UTF-8.
fn decoded_name<'a>(raw: &'a [u8], fallback: &'a str) -> Cow<'a, str> {
    match std::str::from_utf8(raw) {
        Ok(name) => Cow::Borrowed(name),
        Err(_) => Cow::Borrowed(fallback),
    }
}

#[derive(Clone)]
struct EntryData {
    name: String,
    path: String,
    directory: bool,
    size: u64,
    compressed: Option<u64>,
}

struct ArchiveData {
    path: PathBuf,
    format: ArchiveFormat,
    staged_sources: Option<Vec<PathBuf>>,
    directories: HashMap<String, Vec<EntryData>>,
    tree: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveFormat {
    Zip,
    SevenZip,
    Rar,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
}

impl ArchiveFormat {
    fn detect(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .context("文件名无效")?
            .to_string_lossy()
            .to_lowercase();
        if name.ends_with(".zip") {
            Ok(Self::Zip)
        } else if name.ends_with(".7z") {
            Ok(Self::SevenZip)
        } else if name.ends_with(".rar") {
            Ok(Self::Rar)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Ok(Self::TarGz)
        } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") || name.ends_with(".tbz") {
            Ok(Self::TarBz2)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Ok(Self::TarXz)
        } else if name.ends_with(".tar") {
            Ok(Self::Tar)
        } else {
            bail!("不支持的压缩包格式")
        }
    }
}

#[derive(Clone)]
struct FileInfo {
    name: String,
    directory: bool,
    size: u64,
    compressed: Option<u64>,
}

fn tar_reader(path: &Path, format: ArchiveFormat) -> Result<Box<dyn Read>> {
    let file = File::open(path)?;
    Ok(match format {
        ArchiveFormat::Tar => Box::new(file),
        ArchiveFormat::TarGz => Box::new(flate2::read::MultiGzDecoder::new(file)),
        ArchiveFormat::TarBz2 => Box::new(bzip2::read::MultiBzDecoder::new(file)),
        ArchiveFormat::TarXz => Box::new(xz2::read::XzDecoder::new_multi_decoder(file)),
        _ => bail!("不是 TAR 格式"),
    })
}

fn tar_name(entry: &tar::Entry<'_, Box<dyn Read>>) -> Result<String> {
    let path = entry.path()?;
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                parts.push(part.to_str().context("TAR 文件名不是有效的 UTF-8")?)
            }
            _ => bail!("压缩包包含不安全的路径：{}", path.display()),
        }
    }
    let name = parts.join("/");
    if name.is_empty() {
        return Ok(String::new());
    }
    if name.contains('\\') {
        bail!("压缩包包含不安全的路径：{name}");
    }
    Ok(name)
}

fn list_files(path: &Path, format: ArchiveFormat) -> Result<Vec<FileInfo>> {
    match format {
        ArchiveFormat::Zip => {
            let mut zip = ZipArchive::new(File::open(path)?).context("无法读取 ZIP 压缩包")?;
            let mut files = Vec::with_capacity(zip.len());
            for i in 0..zip.len() {
                let entry = zip.by_index(i)?;
                files.push(FileInfo {
                    name: decoded_name(entry.name_raw(), entry.name()).into_owned(),
                    directory: entry.is_dir(),
                    size: entry.size(),
                    compressed: Some(entry.compressed_size()),
                });
            }
            Ok(files)
        }
        ArchiveFormat::Rar => {
            let mut reader = rar::Reader::open(path)?;
            let mut files = Vec::new();
            while let Some(entry) = reader.next()? {
                files.push(entry);
            }
            Ok(files)
        }
        ArchiveFormat::SevenZip => {
            let reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
            reader
                .archive()
                .files
                .iter()
                .filter(|entry| !(entry.name().is_empty() && entry.is_directory()))
                .map(|entry| {
                    let mode = (entry.windows_attributes >> 16) & 0xf000;
                    if entry.is_anti_item
                        || (mode != 0 && mode != 0x8000 && mode != 0x4000 && mode != 0xa000)
                    {
                        bail!("暂不支持特殊文件：{}", entry.name());
                    }
                    Ok(FileInfo {
                        name: entry.name().to_owned(),
                        directory: entry.is_directory(),
                        size: entry.size(),
                        compressed: None,
                    })
                })
                .collect()
        }
        _ => {
            let mut tar = tar::Archive::new(tar_reader(path, format)?);
            let mut files = Vec::new();
            for entry in tar.entries()? {
                let entry = entry?;
                let kind = entry.header().entry_type();
                let name = tar_name(&entry)?;
                if name.is_empty() && kind.is_dir() {
                    continue;
                }
                if !kind.is_dir() && !kind.is_file() && !kind.is_symlink() {
                    bail!("暂不支持链接或特殊文件：{name}");
                }
                files.push(FileInfo {
                    name,
                    directory: kind.is_dir(),
                    size: entry.size(),
                    compressed: None,
                });
            }
            Ok(files)
        }
    }
}

fn read_archive(path: &Path) -> Result<ArchiveData> {
    let format = ArchiveFormat::detect(path)?;
    let files = list_files(path, format)?;
    Ok(build_archive_data(path.to_owned(), format, files, None))
}

fn build_archive_data(
    path: PathBuf,
    format: ArchiveFormat,
    files: Vec<FileInfo>,
    staged_sources: Option<Vec<PathBuf>>,
) -> ArchiveData {
    let mut directories: HashMap<String, Vec<EntryData>> = HashMap::new();
    let mut tree = BTreeSet::new();
    directories.entry(String::new()).or_default();
    for file in files {
        let name = file.name.as_str();
        if name.starts_with('/')
            || name.contains('\\')
            || name.split('/').any(|part| part == "." || part == "..")
        {
            continue;
        }
        let parts: Vec<_> = name.split('/').filter(|part| !part.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let directory_count = if file.directory {
            parts.len()
        } else {
            parts.len() - 1
        };
        let mut parent = String::new();
        for part in &parts[..directory_count] {
            let child = format!("{parent}{part}/");
            if tree.insert(child.clone()) {
                directories
                    .entry(parent.clone())
                    .or_default()
                    .push(EntryData {
                        name: (*part).to_owned(),
                        path: child.clone(),
                        directory: true,
                        size: 0,
                        compressed: None,
                    });
                directories.entry(child.clone()).or_default();
            }
            parent = child;
        }
        if !file.directory {
            directories.entry(parent).or_default().push(EntryData {
                name: parts.last().unwrap().to_string(),
                path: name.to_owned(),
                directory: false,
                size: file.size,
                compressed: file.compressed,
            });
        }
    }
    for entries in directories.values_mut() {
        entries.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| a.name.cmp(&b.name))
        });
    }
    ArchiveData {
        path,
        format,
        staged_sources,
        directories,
        tree: tree.into_iter().collect(),
    }
}

fn create_archive(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("无法创建压缩包（文件可能已存在）")?;
    ZipWriter::new(file).finish()?;
    Ok(())
}

struct SourceEntry {
    path: PathBuf,
    name: String,
    directory: bool,
    size: u64,
    symlink: Option<String>,
}

fn collect_archive_inputs(files: &[PathBuf]) -> Result<Vec<SourceEntry>> {
    if files.is_empty() {
        bail!("没有要打包的文件");
    }
    let mut names = HashSet::new();
    let mut pending = Vec::new();
    for file in files {
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .context("文件名不是有效的 UTF-8")?;
        if name.contains('\\') || name == "." || name == ".." {
            bail!("文件名不适合放入压缩包：{name}");
        }
        if !names.insert(name.to_owned()) {
            bail!("拖入的文件包含重复文件名：{name}");
        }
        pending.push((file.clone(), name.to_owned()));
    }
    let mut entries = Vec::new();
    while let Some((path, name)) = pending.pop() {
        let metadata =
            fs::symlink_metadata(&path).with_context(|| format!("无法读取 {}", path.display()))?;
        let symlink = if metadata.file_type().is_symlink() {
            Some(
                fs::read_link(&path)?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| {
                        anyhow::anyhow!("符号链接目标不是有效的 UTF-8：{}", path.display())
                    })?,
            )
        } else {
            None
        };
        if symlink.is_none() && !metadata.is_dir() && !metadata.is_file() {
            bail!("暂不支持特殊文件：{}", path.display());
        }
        let directory = metadata.is_dir();
        if directory {
            for child in
                fs::read_dir(&path).with_context(|| format!("无法读取文件夹 {}", path.display()))?
            {
                let child = child?;
                let child_name = child
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("文件名不是有效的 UTF-8"))?;
                if child_name.contains('\\') {
                    bail!("文件名不适合放入压缩包：{child_name}");
                }
                pending.push((child.path(), format!("{name}/{child_name}")));
            }
        }
        entries.push(SourceEntry {
            path,
            name: if directory { format!("{name}/") } else { name },
            directory,
            size: metadata.len(),
            symlink,
        });
    }
    Ok(entries)
}

fn stage_archive_inputs(files: Vec<PathBuf>) -> Result<ArchiveData> {
    let entries = collect_archive_inputs(&files)?;
    let files_info = entries
        .into_iter()
        .map(|entry| FileInfo {
            name: entry.name,
            directory: entry.directory,
            size: entry.size,
            compressed: None,
        })
        .collect();
    Ok(build_archive_data(
        PathBuf::new(),
        ArchiveFormat::Zip,
        files_info,
        Some(files),
    ))
}

fn create_archive_from_files(path: &Path, files: &[PathBuf], level: i32) -> Result<()> {
    if ![0, 1, 6, 9].contains(&level) {
        bail!("无效的压缩程度");
    }
    let format = ArchiveFormat::detect(path)?;
    if format == ArchiveFormat::Rar {
        bail!("暂不支持创建 RAR 压缩包");
    }
    let entries = collect_archive_inputs(files)?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    match format {
        ArchiveFormat::Zip => {
            let options = if level == 0 {
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
            } else {
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Deflated)
                    .compression_level(Some(level.into()))
            };
            let mut writer = ZipWriter::new(&mut temp);
            for entry in entries {
                if let Some(target) = entry.symlink {
                    writer.add_symlink(&entry.name, target, options)?;
                } else if entry.directory {
                    writer.add_directory(&entry.name, options)?;
                } else {
                    writer.start_file(&entry.name, options)?;
                    io::copy(
                        &mut File::open(&entry.path)
                            .with_context(|| format!("无法读取 {}", entry.path.display()))?,
                        &mut writer,
                    )?;
                }
            }
            writer.finish()?;
        }
        ArchiveFormat::SevenZip => {
            let mut writer = sevenz_rust2::ArchiveWriter::new(&mut temp)?;
            if level == 0 {
                writer.set_content_methods(vec![sevenz_rust2::EncoderMethod::COPY.into()]);
            } else {
                writer.set_content_methods(vec![
                    sevenz_rust2::encoder_options::Lzma2Options::from_level(level as u32).into(),
                ]);
            }
            for entry in entries {
                let mut item = if entry.directory {
                    sevenz_rust2::ArchiveEntry::new_directory(entry.name.trim_end_matches('/'))
                } else {
                    sevenz_rust2::ArchiveEntry::new_file(&entry.name)
                };
                // sevenz-rust2 0.20 writes the anti-item bit inverted for empty streams.
                // Directories have no stream, so set this to produce a normal directory.
                if entry.directory {
                    item.is_anti_item = true;
                }
                if let Some(target) = entry.symlink {
                    item.has_windows_attributes = true;
                    item.windows_attributes = (0o120777 << 16) | 0x8000;
                    writer.push_archive_entry(item, Some(io::Cursor::new(target.into_bytes())))?;
                } else if entry.directory {
                    writer.push_archive_entry::<File>(item, None)?;
                } else {
                    writer.push_archive_entry(
                        item,
                        Some(
                            File::open(&entry.path)
                                .with_context(|| format!("无法读取 {}", entry.path.display()))?,
                        ),
                    )?;
                }
            }
            writer.finish()?;
        }
        ArchiveFormat::Tar => {
            write_tar(&mut temp, entries)?;
        }
        ArchiveFormat::TarGz => {
            let encoder =
                flate2::write::GzEncoder::new(&mut temp, flate2::Compression::new(level as u32));
            write_tar(encoder, entries)?.finish()?;
        }
        ArchiveFormat::TarBz2 => {
            let encoder = bzip2::write::BzEncoder::new(
                &mut temp,
                bzip2::Compression::new((level as u32).max(1)),
            );
            write_tar(encoder, entries)?.finish()?;
        }
        ArchiveFormat::TarXz => {
            let encoder = xz2::write::XzEncoder::new(&mut temp, level as u32);
            write_tar(encoder, entries)?.finish()?;
        }
        ArchiveFormat::Rar => unreachable!(),
    }
    temp.persist_noclobber(path)
        .map_err(|error| error.error)
        .context("无法创建压缩包（文件可能已存在）")?;
    Ok(())
}

fn write_tar<W: io::Write>(output: W, entries: Vec<SourceEntry>) -> Result<W> {
    let mut writer = tar::Builder::new(output);
    for entry in entries {
        if let Some(target) = entry.symlink {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            writer.append_link(&mut header, &entry.name, target)?;
        } else if entry.directory {
            writer.append_dir(&entry.name, &entry.path)?;
        } else {
            writer.append_file(&entry.name, &mut File::open(&entry.path)?)?;
        }
    }
    Ok(writer.into_inner()?)
}

fn compression_target(destination: &Path, name: &str, format: &str) -> Result<PathBuf> {
    if !destination.is_dir() {
        bail!("保存位置不是已有文件夹：{}", destination.display());
    }
    let name = name.trim();
    let mut parts = Path::new(name).components();
    if name.contains(['/', '\\'])
        || !matches!(parts.next(), Some(Component::Normal(_)))
        || parts.next().is_some()
    {
        bail!("压缩包名称无效");
    }
    if !["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"].contains(&format) {
        bail!("不支持的压缩格式");
    }
    let lower = name.to_ascii_lowercase();
    let suffix = format!(".{format}");
    let name = if lower.ends_with(&suffix) {
        name.to_owned()
    } else {
        if [
            ".zip", ".7z", ".rar", ".tar", ".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".tbz",
            ".tbz2", ".txz",
        ]
        .iter()
        .any(|s| lower.ends_with(s))
        {
            bail!("文件扩展名与所选压缩格式不一致");
        }
        format!("{name}{suffix}")
    };
    Ok(destination.join(name))
}

fn add_files(path: &Path, directory: &str, files: &[PathBuf]) -> Result<()> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let mut names: HashSet<String> = (0..archive.len())
        .map(|i| {
            archive
                .by_index(i)
                .map(|f| decoded_name(f.name_raw(), f.name()).into_owned())
        })
        .collect::<zip::result::ZipResult<_>>()?;
    for file in files {
        let name = file
            .file_name()
            .context("文件名无效")?
            .to_str()
            .context("文件名不是有效的 UTF-8")?;
        if !names.insert(format!("{directory}{name}")) {
            bail!("压缩包中已存在同名文件：{name}");
        }
    }
    drop(archive);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    io::copy(&mut File::open(path)?, &mut temp)?;
    let mut writer = ZipWriter::new_append(temp.reopen()?)?;
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for file in files {
        let name = format!("{directory}{}", file.file_name().unwrap().to_str().unwrap());
        let metadata = fs::symlink_metadata(file)?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(file)?
                .into_os_string()
                .into_string()
                .map_err(|_| anyhow::anyhow!("符号链接目标不是有效的 UTF-8：{}", file.display()))?;
            writer.add_symlink(name, target, options)?;
            continue;
        }
        if !metadata.is_file() {
            bail!("暂不支持添加文件夹或特殊文件：{}", file.display());
        }
        writer.start_file(name, options)?;
        io::copy(
            &mut File::open(file).with_context(|| format!("无法读取 {}", file.display()))?,
            &mut writer,
        )?;
    }
    writer.finish()?;
    temp.persist(path)
        .map_err(|e| e.error)
        .context("无法保存压缩包")?;
    Ok(())
}

fn validate_targets(files: &[FileInfo], destination: &Path) -> Result<Vec<PathBuf>> {
    let mut targets = Vec::with_capacity(files.len());
    let mut file_paths = HashSet::new();
    for entry in files {
        let relative = Path::new(&entry.name);
        if entry.name.contains('\\')
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || relative.as_os_str().is_empty()
        {
            bail!("压缩包包含不安全的路径：{}", entry.name);
        }
        let target = destination.join(relative);
        if !entry.directory && !file_paths.insert(target.clone()) {
            bail!("压缩包包含同名文件：{}", entry.name);
        }
        if let Ok(metadata) = fs::symlink_metadata(&target) {
            if !entry.directory || !metadata.is_dir() || metadata.file_type().is_symlink() {
                bail!("目标路径已存在：{}", target.display());
            }
        }
        for ancestor in target.ancestors().take_while(|p| *p != destination) {
            if fs::symlink_metadata(ancestor)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                bail!("目标路径包含符号链接：{}", ancestor.display());
            }
        }
        targets.push(target);
    }
    for (entry, target) in files.iter().zip(&targets) {
        if (entry.directory && file_paths.contains(target))
            || target
                .ancestors()
                .skip(1)
                .take_while(|p| *p != destination)
                .any(|p| file_paths.contains(p))
        {
            bail!("压缩包中的文件与目录路径冲突：{}", target.display());
        }
    }
    Ok(targets)
}

fn write_extracted(target: &Path, directory: bool, reader: &mut dyn Read) -> io::Result<()> {
    if directory {
        fs::create_dir_all(target)?;
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)?;
        io::copy(reader, &mut output)?;
    }
    Ok(())
}

#[cfg(test)]
fn extract_archive(path: &Path, destination: &Path) -> Result<()> {
    extract_selection(path, destination, None)
}

fn extract_temporary_file(archive: &Path, selection: &str) -> Result<(tempfile::TempDir, PathBuf)> {
    if selection.is_empty() || selection.ends_with('/') {
        bail!("请选择文件");
    }
    extract_temporary_selection(archive, selection, &mut |_, _| {})
}

fn extract_temporary_selection(
    archive: &Path,
    selection: &str,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(tempfile::TempDir, PathBuf)> {
    if selection.is_empty() {
        bail!("请选择文件或文件夹");
    }
    let name = Path::new(selection.trim_end_matches('/'))
        .file_name()
        .context("文件名无效")?;
    let directory = tempfile::Builder::new()
        .prefix("sugar-extract-")
        .tempdir()?;
    extract_selection_with_progress(archive, directory.path(), Some(selection), progress)?;
    let file = directory.path().join(name);
    Ok((directory, file))
}

fn open_with_default_app(path: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    let status = command.arg(path).status().context("无法启动默认程序")?;
    if !status.success() {
        bail!("无法使用默认程序打开文件");
    }
    Ok(())
}

// A selected directory retains its own name, but drops its parents in the archive.
#[cfg(test)]
fn extract_selection(path: &Path, destination: &Path, selection: Option<&str>) -> Result<()> {
    extract_selection_with_progress(path, destination, selection, &mut |_, _| {})
}

struct ProgressReader<'a> {
    reader: &'a mut dyn Read,
    completed: &'a mut u64,
    total: u64,
    progress: &'a mut dyn FnMut(u64, u64),
}

impl Read for ProgressReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.reader.read(buffer)?;
        *self.completed = self.completed.saturating_add(count as u64);
        (self.progress)(*self.completed, self.total);
        Ok(count)
    }
}

fn extract_selection_with_progress(
    path: &Path,
    destination: &Path,
    selection: Option<&str>,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<()> {
    let format = ArchiveFormat::detect(path)?;
    let files = list_files(path, format)?;
    let parent = selection
        .map(|selected| {
            selected
                .trim_end_matches('/')
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent)
        })
        .unwrap_or("");
    let prefix = if parent.is_empty() {
        String::new()
    } else {
        format!("{parent}/")
    };
    let selected: Vec<usize> = files
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let matches = selection.is_none_or(|selected| {
                if selected.ends_with('/') {
                    entry.name.trim_end_matches('/') == selected.trim_end_matches('/')
                        || entry.name.starts_with(selected)
                } else {
                    entry.name == selected && !entry.directory
                }
            });
            matches.then_some(i)
        })
        .collect();
    if selection.is_some() && selected.is_empty() {
        bail!("所选项目不存在，压缩包可能已更改");
    }
    let mapped: Vec<FileInfo> = selected
        .iter()
        .map(|&i| {
            let mut entry = files[i].clone();
            entry.name = entry
                .name
                .strip_prefix(&prefix)
                .unwrap_or(&entry.name)
                .to_owned();
            entry
        })
        .collect();
    let checked = validate_targets(&mapped, destination)?;
    let total = mapped
        .iter()
        .filter(|entry| !entry.directory)
        .fold(0u64, |sum, entry| sum.saturating_add(entry.size));
    let mut completed = 0u64;
    progress(0, total);
    let mut targets = vec![None; files.len()];
    for (index, target) in selected.iter().copied().zip(checked) {
        targets[index] = Some(target);
    }
    match format {
        ArchiveFormat::Zip => {
            let mut zip = ZipArchive::new(File::open(path)?)?;
            for &i in &selected {
                let entry = zip.by_index(i)?;
                if entry.is_symlink() {
                    bail!("暂不支持解压符号链接：{}", entry.name());
                }
                if decoded_name(entry.name_raw(), entry.name()) != files[i].name
                    || entry.is_dir() != files[i].directory
                {
                    bail!("压缩包在解压过程中发生变化");
                }
            }
            for &i in &selected {
                let mut file = zip.by_index(i)?;
                let mut reader = ProgressReader {
                    reader: &mut file,
                    completed: &mut completed,
                    total,
                    progress,
                };
                write_extracted(
                    targets[i].as_ref().unwrap(),
                    files[i].directory,
                    &mut reader,
                )?;
            }
        }
        ArchiveFormat::Rar => {
            let mut reader = rar::Reader::open(path)?;
            let mut index = 0;
            while let Some(entry) = reader.next()? {
                let expected = files.get(index).context("压缩包在解压过程中发生变化")?;
                if entry.name != expected.name
                    || entry.directory != expected.directory
                    || entry.size != expected.size
                {
                    bail!("压缩包在解压过程中发生变化");
                }
                if let Some(target) = &targets[index] {
                    let mut input = ProgressReader {
                        reader: &mut reader,
                        completed: &mut completed,
                        total,
                        progress,
                    };
                    write_extracted(target, expected.directory, &mut input)?;
                }
                index += 1;
            }
            if index != files.len() {
                bail!("压缩包在解压过程中发生变化");
            }
        }
        ArchiveFormat::SevenZip => {
            let mut reader =
                sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
            for (entry, target) in reader
                .archive()
                .files
                .iter()
                .filter(|e| !(e.name().is_empty() && e.is_directory()))
                .zip(&targets)
            {
                if target.is_some() && (entry.windows_attributes >> 16) & 0xf000 == 0xa000 {
                    bail!("暂不支持解压符号链接：{}", entry.name());
                }
            }
            let mut expected: HashMap<&str, VecDeque<(Option<&PathBuf>, bool)>> = HashMap::new();
            for (entry, target) in files.iter().zip(&targets) {
                expected
                    .entry(entry.name.as_str())
                    .or_default()
                    .push_back((target.as_ref(), entry.directory));
            }
            reader.for_each_entries(|entry, input| {
                if entry.name().is_empty() && entry.is_directory() {
                    return Ok(true);
                }
                let (target, directory) = expected
                    .get_mut(entry.name())
                    .and_then(VecDeque::pop_front)
                    .ok_or_else(|| io::Error::other("压缩包在解压过程中发生变化"))?;
                if directory != entry.is_directory() {
                    return Err(io::Error::other("压缩包在解压过程中发生变化").into());
                }
                if let Some(target) = target {
                    let mut reader = ProgressReader {
                        reader: input,
                        completed: &mut completed,
                        total,
                        progress,
                    };
                    write_extracted(target, directory, &mut reader)?;
                } else {
                    io::copy(input, &mut io::sink())?;
                }
                Ok(true)
            })?;
            if expected.values().any(|entries| !entries.is_empty()) {
                bail!("压缩包在解压过程中发生变化");
            }
        }
        _ => {
            let mut tar = tar::Archive::new(tar_reader(path, format)?);
            let mut index = 0;
            for entry in tar.entries()? {
                let mut entry = entry?;
                let name = tar_name(&entry)?;
                if name.is_empty() && entry.header().entry_type().is_dir() {
                    continue;
                }
                let expected = files.get(index).context("压缩包在解压过程中发生变化")?;
                let kind = entry.header().entry_type();
                if name != expected.name
                    || (expected.directory && !kind.is_dir())
                    || (!expected.directory && !kind.is_file() && !kind.is_symlink())
                {
                    bail!("压缩包在解压过程中发生变化");
                }
                if let Some(target) = &targets[index] {
                    if kind.is_symlink() {
                        bail!("暂不支持解压符号链接：{name}");
                    }
                    let mut reader = ProgressReader {
                        reader: &mut entry,
                        completed: &mut completed,
                        total,
                        progress,
                    };
                    write_extracted(target, expected.directory, &mut reader)?;
                }
                index += 1;
            }
            if index != files.len() {
                bail!("压缩包在解压过程中发生变化");
            }
        }
    }
    Ok(())
}

fn remaining_time(completed: u64, total: u64, elapsed: Duration) -> String {
    if completed == 0 || total == 0 || elapsed < Duration::from_secs(1) {
        return "预计剩余时间：计算中…".into();
    }
    if completed >= total {
        return "预计剩余时间：即将完成".into();
    }
    let seconds =
        ((total - completed) as f64 * elapsed.as_secs_f64() / completed as f64).ceil() as u64;
    if seconds < 60 {
        format!("预计剩余时间：{seconds} 秒")
    } else if seconds < 3600 {
        format!("预计剩余时间：{} 分 {} 秒", seconds / 60, seconds % 60)
    } else {
        format!(
            "预计剩余时间：{} 小时 {} 分",
            seconds / 3600,
            seconds % 3600 / 60
        )
    }
}

fn archive_folder_name(path: &Path) -> String {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let lower = filename.to_lowercase();
    let suffixes = [
        ".tar.bz2", ".tar.gz", ".tar.xz", ".tbz2", ".tgz", ".tbz", ".txz", ".zip", ".tar", ".7z",
        ".rar",
    ];
    let name = suffixes
        .iter()
        .find(|suffix| lower.ends_with(**suffix))
        .map_or(filename.as_ref(), |suffix| {
            &filename[..filename.len() - suffix.len()]
        });
    if name.is_empty() || name == "." || name == ".." {
        "解压文件".to_owned()
    } else {
        name.to_owned()
    }
}

fn prepare_destination(destination: &Path, extra: Option<&str>) -> Result<PathBuf> {
    if !destination.is_dir() {
        bail!("解压位置不是已有文件夹：{}", destination.display());
    }
    let Some(name) = extra else {
        return Ok(destination.to_owned());
    };
    let mut components = Path::new(name).components();
    if name.trim().is_empty()
        || name.contains(['/', '\\', '\0'])
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        bail!("文件夹名称无效，请输入单个文件夹名称，不要包含路径分隔符");
    }
    let target = destination.join(name);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => bail!("额外文件夹路径已存在且不是普通文件夹：{}", target.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&target)?,
        Err(error) => return Err(error.into()),
    }
    Ok(target)
}

#[derive(Clone, PartialEq, Eq)]
struct ArchiveVersion {
    length: u64,
    modified: SystemTime,
}

impl ArchiveVersion {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path).context("无法读取压缩包信息")?;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
        })
    }
}

struct PreparedDrag {
    archive: Arc<ArchiveData>,
    item: String,
    version: ArchiveVersion,
    path: PathBuf,
}

impl PreparedDrag {
    fn matches(&self, archive: &Arc<ArchiveData>, item: &str, version: &ArchiveVersion) -> bool {
        Arc::ptr_eq(&self.archive, archive)
            && self.item == item
            && self.version == *version
            && self.path.exists()
    }
}

fn pudding_cli(args: &[std::ffi::OsString]) -> Result<()> {
    let action = args.first().and_then(|s| s.to_str()).context("缺少操作")?;
    match action {
        "--protocol-version" if args.len() == 1 => println!("sugar-pudding-2"),
        "extract" if args.len() == 3 => {
            let archive = Path::new(&args[1]);
            let destination = Path::new(&args[2]);
            if !destination.is_dir() {
                bail!("解压目标不是文件夹");
            }
            extract_selection_with_progress(archive, destination, None, &mut |_, _| {})?;
        }
        "stage" if args.len() >= 2 => {
            std::process::Command::new(std::env::current_exe()?.with_file_name(if cfg!(windows) {
                "sugar-archive.exe"
            } else {
                "sugar-archive"
            }))
            .arg("--pudding-stage")
            .args(&args[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("无法启动 Sugar")?;
        }
        "compress" if args.len() >= 3 => {
            let destination = Path::new(&args[1]);
            let sources: Vec<PathBuf> = args[2..].iter().map(PathBuf::from).collect();
            create_archive_from_files(destination, &sources, 6)?;
        }
        _ => bail!("无效的操作参数"),
    }
    Ok(())
}

mod app;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args
        .first()
        .and_then(|s| Path::new(s).file_stem())
        .and_then(|s| s.to_str())
        == Some("sugar-archive-pudding")
    {
        return pudding_cli(&args[1..]);
    }
    app::run()
}

#[cfg(test)]
mod tests {
    #[test]
    fn creates_and_extracts_all_writable_formats() -> anyhow::Result<()> {
        use std::{fs, path::Path};
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input");
        fs::create_dir(&input)?;
        fs::create_dir(input.join("empty"))?;
        fs::write(input.join("data.txt"), b"archive content")?;
        for format in ["zip", "7z", "tar", "tar.gz", "tar.bz2", "tar.xz"] {
            let path = super::compression_target(dir.path(), "archive", format)?;
            super::create_archive_from_files(&path, std::slice::from_ref(&input), 6)?;
            super::read_archive(&path)?;
            let out = dir.path().join(format!("out-{format}"));
            fs::create_dir(&out)?;
            super::extract_archive(&path, &out)?;
            assert_eq!(
                fs::read(out.join("input/data.txt"))?,
                b"archive content",
                "{format}"
            );
            assert!(out.join("input/empty").is_dir(), "{format}");
            assert!(
                super::create_archive_from_files(&path, std::slice::from_ref(&input), 6).is_err()
            );
        }
        assert!(super::compression_target(Path::new(dir.path()), "archive.zip", "7z").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn additional_formats_preserve_symlinks() -> anyhow::Result<()> {
        use std::fs;
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input");
        fs::create_dir(&input)?;
        fs::write(input.join("data"), b"hello")?;
        std::os::unix::fs::symlink("../missing", input.join("link"))?;
        for format in ["7z", "tar", "tar.gz", "tar.bz2", "tar.xz"] {
            let path = super::compression_target(dir.path(), "links", format)?;
            super::create_archive_from_files(&path, std::slice::from_ref(&input), 6)?;
            super::read_archive(&path)?;
            if format == "7z" {
                let archive =
                    sevenz_rust2::ArchiveReader::open(&path, sevenz_rust2::Password::empty())?;
                let link = archive
                    .archive()
                    .files
                    .iter()
                    .find(|e| e.name() == "input/link")
                    .unwrap();
                assert_eq!((link.windows_attributes >> 16) & 0xf000, 0xa000);
            } else {
                let format = super::ArchiveFormat::detect(&path)?;
                let mut tar = tar::Archive::new(super::tar_reader(&path, format)?);
                let mut found = false;
                for entry in tar.entries()? {
                    let entry = entry?;
                    if entry.path()? == std::path::Path::new("input/link") {
                        assert!(entry.header().entry_type().is_symlink());
                        assert_eq!(
                            entry.link_name()?.unwrap(),
                            std::path::Path::new("../missing")
                        );
                        found = true;
                    }
                }
                assert!(found);
            }
            let out = dir.path().join(format!("link-out-{format}"));
            fs::create_dir(&out)?;
            assert!(super::extract_selection(&path, &out, Some("input/link")).is_err());
            super::extract_selection(&path, &out, Some("input/data"))?;
            assert_eq!(fs::read(out.join("data"))?, b"hello");
        }
        Ok(())
    }

    #[test]
    fn rar_browse_and_extract() -> anyhow::Result<()> {
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/stored.rar"
        ));
        let files = super::list_files(path, super::ArchiveFormat::detect(path)?)?;
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "hello.txt");
        let dir = tempfile::tempdir()?;
        super::extract_archive(path, dir.path())?;
        assert_eq!(std::fs::read(dir.path().join("hello.txt"))?, b"hello rar\n");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn zip_preserves_symlinks_without_following_them() -> anyhow::Result<()> {
        use std::io::Read;
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input");
        std::fs::create_dir(&input)?;
        std::os::unix::fs::symlink("../missing", input.join("link"))?;
        std::os::unix::fs::symlink(".", input.join("loop"))?;
        let archive = dir.path().join("test.zip");
        super::create_archive_from_files(&archive, &[input], 6)?;
        let mut zip = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
        assert_eq!(zip.len(), 3);
        let mut link = zip.by_name("input/link")?;
        assert!(link.is_symlink());
        let mut target = String::new();
        link.read_to_string(&mut target)?;
        assert_eq!(target, "../missing");
        Ok(())
    }

    use super::*;

    #[test]
    fn pudding_cli_round_trip_and_no_overwrite() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("hello.txt");
        let archive = dir.path().join("archive.zip");
        let out = dir.path().join("out");
        fs::write(&input, "hello")?;
        fs::create_dir(&out)?;
        let args = |action: &str, paths: &[&Path]| -> Vec<std::ffi::OsString> {
            std::iter::once(std::ffi::OsString::from(action))
                .chain(paths.iter().map(|p| p.as_os_str().to_owned()))
                .collect()
        };
        pudding_cli(&args("compress", &[&archive, &input]))?;
        assert!(pudding_cli(&args("compress", &[&archive, &input])).is_err());
        pudding_cli(&args("extract", &[&archive, &out]))?;
        assert_eq!(fs::read(out.join("hello.txt"))?, b"hello");
        assert!(pudding_cli(&args("extract", &[&archive, &out])).is_err());
        Ok(())
    }

    #[test]
    fn prepared_drag_reuses_only_the_same_unchanged_archive_and_entry() -> Result<()> {
        use std::io::Write;
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("文件.txt");
        fs::write(&source, b"original")?;
        let path = dir.path().join("archive.zip");
        create_archive_from_files(&path, &[source], 6)?;
        let archive = Arc::new(read_archive(&path)?);
        let version = ArchiveVersion::read(&path)?;
        let (temporary, output) = extract_temporary_selection(&path, "文件.txt", &mut |_, _| {})?;
        let prepared = PreparedDrag {
            archive: archive.clone(),
            item: "文件.txt".into(),
            version: version.clone(),
            path: output,
        };
        assert!(prepared.matches(&archive, "文件.txt", &version));
        assert!(!prepared.matches(&archive, "other.txt", &version));
        assert!(!prepared.matches(&Arc::new(read_archive(&path)?), "文件.txt", &version));
        OpenOptions::new()
            .append(true)
            .open(&path)?
            .write_all(b"changed")?;
        assert!(!prepared.matches(&archive, "文件.txt", &ArchiveVersion::read(&path)?));
        drop(temporary);
        assert!(!prepared.matches(&archive, "文件.txt", &version));
        Ok(())
    }

    #[test]
    fn prepares_empty_unicode_directory_and_rejects_missing_drag_items() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("empty.zip");
        let mut writer = ZipWriter::new(File::create(&archive)?);
        writer.add_directory("top/空 文件夹/", SimpleFileOptions::default())?;
        writer.finish()?;
        let (temporary, folder) =
            extract_temporary_selection(&archive, "top/空 文件夹/", &mut |_, _| {})?;
        assert_eq!(folder, temporary.path().join("空 文件夹"));
        assert!(folder.is_dir());
        assert_eq!(fs::read_dir(&folder)?.count(), 0);
        for selection in ["", "missing.txt", "missing/"] {
            assert!(extract_temporary_selection(&archive, selection, &mut |_, _| {}).is_err());
        }
        Ok(())
    }

    #[test]
    fn create_zip_from_dropped_files_without_overwriting() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let first = dir.path().join("a.txt");
        let second = dir.path().join("b.txt");
        fs::write(&first, b"first")?;
        fs::write(&second, b"second")?;
        let archive = dir.path().join("files.zip");
        create_archive_from_files(&archive, &[first.clone(), second.clone()], 6)?;
        let mut zip = ZipArchive::new(File::open(&archive)?)?;
        let mut contents = String::new();
        zip.by_name("a.txt")?.read_to_string(&mut contents)?;
        assert_eq!(contents, "first");
        assert_eq!(zip.len(), 2);
        let before = fs::read(&archive)?;
        assert!(create_archive_from_files(&archive, std::slice::from_ref(&first), 6).is_err());
        assert_eq!(fs::read(&archive)?, before);
        let other = dir.path().join("other");
        fs::create_dir(&other)?;
        fs::create_dir(other.join("empty"))?;
        fs::write(other.join("a.txt"), b"duplicate")?;
        let staged = stage_archive_inputs(vec![other.clone(), second])?;
        assert_eq!(staged.directories[""][0].name, "other");
        assert!(staged.directories["other/"]
            .iter()
            .any(|entry| entry.name == "a.txt"));
        assert!(staged.directories["other/empty/"].is_empty());
        let folder_zip = dir.path().join("folder.zip");
        create_archive_from_files(&folder_zip, std::slice::from_ref(&other), 0)?;
        let mut zip = ZipArchive::new(File::open(folder_zip)?)?;
        assert!(zip.by_name("other/empty/")?.is_dir());
        assert_eq!(zip.by_name("other/a.txt")?.size(), 9);
        assert_eq!(
            zip.by_name("other/a.txt")?.compression(),
            CompressionMethod::Stored
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&first, other.join("link"))?;
            assert!(stage_archive_inputs(vec![other.clone()]).is_ok());
        }
        assert!(create_archive_from_files(
            &dir.path().join("duplicate.zip"),
            &[first, other.join("a.txt")],
            6
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn compression_options_validate_output_and_use_selected_method() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input.txt");
        fs::write(&input, vec![b'a'; 4096])?;
        assert_eq!(
            compression_target(dir.path(), "归档", "zip")?,
            dir.path().join("归档.zip")
        );
        assert_eq!(
            compression_target(dir.path(), "归档.ZIP", "zip")?,
            dir.path().join("归档.ZIP")
        );
        for name in ["", "..", "../escape", "folder/", "folder\\file"] {
            assert!(
                compression_target(dir.path(), name, "zip").is_err(),
                "{name}"
            );
        }
        assert!(compression_target(&dir.path().join("missing"), "a", "zip").is_err());
        for level in [1, 6, 9] {
            let path = dir.path().join(format!("{level}.zip"));
            create_archive_from_files(&path, std::slice::from_ref(&input), level)?;
            let mut zip = ZipArchive::new(File::open(path)?)?;
            assert_eq!(
                zip.by_name("input.txt")?.compression(),
                CompressionMethod::Deflated
            );
        }
        assert!(create_archive_from_files(&dir.path().join("bad.zip"), &[input], 3).is_err());
        Ok(())
    }

    #[test]
    fn extraction_progress_counts_selected_bytes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("progress.zip");
        let mut writer = ZipWriter::new(File::create(&archive)?);
        writer.start_file("folder/selected.txt", SimpleFileOptions::default())?;
        std::io::Write::write_all(&mut writer, &vec![42; 100_000])?;
        writer.start_file("folder/other.txt", SimpleFileOptions::default())?;
        std::io::Write::write_all(&mut writer, b"other")?;
        writer.finish()?;
        let destination = dir.path().join("out");
        fs::create_dir(&destination)?;
        let mut updates = Vec::new();
        extract_selection_with_progress(
            &archive,
            &destination,
            Some("folder/selected.txt"),
            &mut |done, total| updates.push((done, total)),
        )?;
        assert_eq!(updates.first(), Some(&(0, 100_000)));
        assert_eq!(updates.last(), Some(&(100_000, 100_000)));
        assert!(updates.windows(2).all(|pair| pair[0].0 <= pair[1].0));
        assert_eq!(
            remaining_time(0, 100, Duration::from_secs(2)),
            "预计剩余时间：计算中…"
        );
        assert_eq!(
            remaining_time(50, 100, Duration::from_secs(10)),
            "预计剩余时间：10 秒"
        );
        Ok(())
    }

    #[test]
    fn file_icons_follow_extension_and_directory() {
        for (name, directory, expected) in [
            ("photos.jpg", true, "folder"),
            ("PHOTO.JPEG", false, "image"),
            ("song.mp3", false, "audio"),
            ("movie.mp4", false, "video"),
            ("backup.tar.gz", false, "archive"),
            ("main.rs", false, "code"),
            ("report.pdf", false, "document"),
            ("README", false, "file"),
            (".gitignore", false, "file"),
            ("unknown.xyz", false, "file"),
        ] {
            assert_eq!(file_icon(name, directory), expected, "{name}");
        }
    }

    #[test]
    fn opens_selected_file_in_its_own_temporary_directory() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive = dir.path().join("files.zip");
        let mut writer = ZipWriter::new(File::create(&archive)?);
        for name in ["folder/file.txt", "folder/other.txt"] {
            writer.start_file(name, SimpleFileOptions::default())?;
            std::io::Write::write_all(&mut writer, name.as_bytes())?;
        }
        writer.finish()?;
        let (temporary, file) = extract_temporary_file(&archive, "folder/file.txt")?;
        assert_eq!(file, temporary.path().join("file.txt"));
        assert_eq!(fs::read(&file)?, b"folder/file.txt");
        assert_eq!(fs::read_dir(temporary.path())?.count(), 1);
        assert!(extract_temporary_file(&archive, "folder/").is_err());
        assert!(extract_temporary_file(&archive, "missing.txt").is_err());
        drop(temporary);
        assert!(!file.exists());
        Ok(())
    }

    #[test]
    fn prepare_extra_extraction_folder() -> Result<()> {
        let dir = tempfile::tempdir()?;
        assert_eq!(archive_folder_name(Path::new("资料.TAR.GZ")), "资料");
        assert_eq!(archive_folder_name(Path::new("归档.zip")), "归档");
        assert_eq!(archive_folder_name(Path::new(".zip")), "解压文件");
        assert_eq!(prepare_destination(dir.path(), None)?, dir.path());
        let extra = prepare_destination(dir.path(), Some("资料"))?;
        assert!(extra.is_dir());
        assert_eq!(prepare_destination(dir.path(), Some("资料"))?, extra);
        for name in [
            "",
            "   ",
            ".",
            "..",
            "../escape",
            "/tmp",
            "nested/folder",
            "folder/",
            "folder\\nested",
            "invalid\0name",
        ] {
            assert!(
                prepare_destination(dir.path(), Some(name)).is_err(),
                "{name:?}"
            );
        }
        fs::write(dir.path().join("file"), b"existing")?;
        assert!(prepare_destination(dir.path(), Some("file")).is_err());
        assert!(prepare_destination(&dir.path().join("missing"), None).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path(), dir.path().join("link"))?;
            assert!(prepare_destination(dir.path(), Some("link")).is_err());
        }
        Ok(())
    }

    #[test]
    fn extracts_into_custom_named_folder() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("input.txt");
        fs::write(&input, b"custom extraction folder")?;
        let archive = dir.path().join("archive.zip");
        create_archive_from_files(&archive, &[input], 6)?;
        let destination = prepare_destination(dir.path(), Some("整理后的资料 2026"))?;
        extract_archive(&archive, &destination)?;
        assert_eq!(destination, dir.path().join("整理后的资料 2026"));
        assert_eq!(
            fs::read(destination.join("input.txt"))?,
            b"custom extraction folder"
        );
        assert!(!dir.path().join("archive").exists());
        Ok(())
    }

    #[test]
    fn extracts_only_selected_file_or_directory_across_formats() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let names = [
            "top/dir/file.txt",
            "top/dir/nested/other.txt",
            "top/directory/unrelated.txt",
        ];
        for format in ["zip", "tar", "7z"] {
            let path = dir.path().join(format!("select.{format}"));
            match format {
                "zip" => {
                    let mut writer = ZipWriter::new(File::create(&path)?);
                    for name in names {
                        writer.start_file(name, SimpleFileOptions::default())?;
                        std::io::Write::write_all(&mut writer, name.as_bytes())?;
                    }
                    writer.finish()?;
                }
                "tar" => {
                    let mut writer = tar::Builder::new(File::create(&path)?);
                    for name in names {
                        let mut header = tar::Header::new_gnu();
                        header.set_size(name.len() as u64);
                        header.set_mode(0o644);
                        header.set_cksum();
                        writer.append_data(&mut header, name, name.as_bytes())?;
                    }
                    writer.finish()?;
                }
                _ => {
                    let mut writer = sevenz_rust2::ArchiveWriter::create(&path)?;
                    for name in names {
                        writer.push_archive_entry(
                            sevenz_rust2::ArchiveEntry::new_file(name),
                            Some(name.as_bytes()),
                        )?;
                    }
                    writer.finish()?;
                }
            }
            let output = dir.path().join(format!("{format}-file"));
            fs::create_dir(&output)?;
            extract_selection(&path, &output, Some(names[0]))?;
            assert_eq!(fs::read(output.join("file.txt"))?, names[0].as_bytes());
            assert_eq!(fs::read_dir(&output)?.count(), 1);
            assert!(extract_selection(&path, &output, Some(names[0])).is_err());
            let output = dir.path().join(format!("{format}-dir"));
            fs::create_dir(&output)?;
            extract_selection(&path, &output, Some("top/dir/"))?;
            assert_eq!(fs::read(output.join("dir/file.txt"))?, names[0].as_bytes());
            assert_eq!(
                fs::read(output.join("dir/nested/other.txt"))?,
                names[1].as_bytes()
            );
            assert!(!output.join("directory").exists());
            assert!(extract_selection(&path, &output, Some("missing/")).is_err());
            let (temporary, dragged) =
                extract_temporary_selection(&path, "top/dir/", &mut |_, _| {})?;
            assert_eq!(dragged, temporary.path().join("dir"));
            assert_eq!(fs::read(dragged.join("file.txt"))?, names[0].as_bytes());
            assert_eq!(
                fs::read(dragged.join("nested/other.txt"))?,
                names[1].as_bytes()
            );
            assert_eq!(fs::read_dir(temporary.path())?.count(), 1);
            drop(temporary);
            assert!(!dragged.exists());
        }
        Ok(())
    }

    #[test]
    fn browse_and_extract_tar_variants_and_7z() -> Result<()> {
        use std::io::Write;
        let dir = tempfile::tempdir()?;
        let mut builder = tar::Builder::new(Vec::new());
        let contents = b"hello archive";
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, "folder/中文.txt", &contents[..])?;
        let tar_bytes = builder.into_inner()?;
        for (name, format) in [
            ("a.tar", ArchiveFormat::Tar),
            ("a.tar.gz", ArchiveFormat::TarGz),
            ("a.tar.bz2", ArchiveFormat::TarBz2),
            ("a.tar.xz", ArchiveFormat::TarXz),
        ] {
            let path = dir.path().join(name);
            let data = match format {
                ArchiveFormat::Tar => tar_bytes.clone(),
                ArchiveFormat::TarGz => {
                    let mut e =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                    e.write_all(&tar_bytes)?;
                    e.finish()?
                }
                ArchiveFormat::TarBz2 => {
                    let mut e =
                        bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
                    e.write_all(&tar_bytes)?;
                    e.finish()?
                }
                ArchiveFormat::TarXz => {
                    let mut e = xz2::write::XzEncoder::new(Vec::new(), 6);
                    e.write_all(&tar_bytes)?;
                    e.finish()?
                }
                _ => unreachable!(),
            };
            fs::write(&path, data)?;
            let archive = read_archive(&path)?;
            assert_eq!(archive.tree, ["folder/"]);
            assert_eq!(archive.directories["folder/"][0].name, "中文.txt");
            let out = dir.path().join(format!("output-{name}"));
            fs::create_dir(&out)?;
            extract_archive(&path, &out)?;
            assert_eq!(fs::read(out.join("folder/中文.txt"))?, contents);
            assert!(extract_archive(&path, &out).is_err());
        }
        let source = dir.path().join("source");
        fs::create_dir(&source)?;
        fs::write(source.join("中文.txt"), contents)?;
        let path = dir.path().join("test.7z");
        sevenz_rust2::compress_to_path(source.join("中文.txt"), &path)?;
        let archive = read_archive(&path)?;
        assert!(archive
            .directories
            .values()
            .flatten()
            .any(|entry| entry.name == "中文.txt"));
        let out = dir.path().join("output-7z");
        fs::create_dir(&out)?;
        extract_archive(&path, &out)?;
        let extracted = out.join(
            archive
                .directories
                .values()
                .flatten()
                .find(|e| e.name == "中文.txt")
                .unwrap()
                .path
                .as_str(),
        );
        assert_eq!(fs::read(extracted)?, contents);
        assert!(extract_archive(&path, &out).is_err());
        Ok(())
    }

    #[test]
    fn reject_unsafe_and_conflicting_paths() {
        let destination = Path::new("/tmp/archive-test-destination");
        for name in [
            "../escape",
            "/absolute",
            "folder/../../escape",
            "folder\\escape",
        ] {
            let files = [FileInfo {
                name: name.into(),
                directory: false,
                size: 0,
                compressed: None,
            }];
            assert!(validate_targets(&files, destination).is_err(), "{name}");
        }
        let files = [
            FileInfo {
                name: "folder".into(),
                directory: false,
                size: 0,
                compressed: None,
            },
            FileInfo {
                name: "folder/file".into(),
                directory: false,
                size: 0,
                compressed: None,
            },
        ];
        assert!(validate_targets(&files, destination).is_err());
    }

    #[test]
    fn unflagged_utf8_names_are_displayed_and_extracted_correctly() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("unflagged.zip");
        let mut writer = ZipWriter::new(File::create(&path)?);
        writer.start_file("资料/中文.txt", SimpleFileOptions::default())?;
        std::io::Write::write_all(&mut writer, b"content")?;
        writer.finish()?;
        let mut bytes = fs::read(&path)?;
        // Clear bit 11 in the local and central directory headers.
        for signature in [b"PK\x03\x04", b"PK\x01\x02"] {
            let offset = bytes
                .windows(4)
                .position(|window| window == signature)
                .unwrap();
            let flags = if signature[2] == 3 {
                offset + 6
            } else {
                offset + 8
            };
            bytes[flags + 1] &= !0x08;
        }
        fs::write(&path, bytes)?;
        let data = read_archive(&path)?;
        assert_eq!(data.tree, ["资料/"]);
        assert_eq!(data.directories["资料/"][0].name, "中文.txt");
        let destination = dir.path().join("output");
        fs::create_dir(&destination)?;
        extract_archive(&path, &destination)?;
        assert_eq!(fs::read(destination.join("资料/中文.txt"))?, b"content");
        assert!(extract_archive(&path, &destination).is_err());
        Ok(())
    }

    #[test]
    fn read_many_entries() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("many.zip");
        let mut writer = ZipWriter::new(File::create(&path)?);
        for index in 0..10_000 {
            writer.start_file(format!("file-{index}.txt"), SimpleFileOptions::default())?;
        }
        writer.finish()?;
        let data = read_archive(&path)?;
        let entries = &data.directories[""];
        assert_eq!(entries.len(), 10_000);
        assert_eq!(entries[9_999].name, "file-9999.txt");
        Ok(())
    }

    #[test]
    fn builds_directory_tree_and_adds_to_selected_directory() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested.zip");
        let mut writer = ZipWriter::new(File::create(&path)?);
        writer.start_file("alpha/beta/inside.txt", SimpleFileOptions::default())?;
        writer.add_directory("empty/", SimpleFileOptions::default())?;
        writer.finish()?;
        let data = read_archive(&path)?;
        assert_eq!(data.tree, ["alpha/", "alpha/beta/", "empty/"]);
        assert_eq!(
            data.directories[""]
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "empty"]
        );
        assert_eq!(data.directories["alpha/beta/"][0].name, "inside.txt");
        assert!(data.directories["empty/"].is_empty());
        let input = dir.path().join("new.txt");
        fs::write(&input, b"nested")?;
        add_files(&path, "alpha/beta/", &[input])?;
        let mut zip = ZipArchive::new(File::open(path)?)?;
        assert!(zip.by_name("alpha/beta/new.txt").is_ok());
        Ok(())
    }

    #[test]
    fn create_add_and_reject_duplicates_without_changing_archive() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let archive_path = dir.path().join("test.zip");
        let input = dir.path().join("hello.txt");
        fs::write(&input, b"hello archive")?;
        create_archive(&archive_path)?;
        add_files(&archive_path, "", std::slice::from_ref(&input))?;
        let before = fs::read(&archive_path)?;
        assert!(add_files(&archive_path, "", &[input]).is_err());
        assert_eq!(before, fs::read(&archive_path)?);
        let mut archive = ZipArchive::new(File::open(&archive_path)?)?;
        let mut content = String::new();
        std::io::Read::read_to_string(&mut archive.by_name("hello.txt")?, &mut content)?;
        assert_eq!(content, "hello archive");
        Ok(())
    }
}
