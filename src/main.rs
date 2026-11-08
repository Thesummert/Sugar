use anyhow::{bail, Context, Result};
use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker};
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

slint::include_modules!();

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
    directories: HashMap<String, Vec<EntryData>>,
    tree: Vec<String>,
}

// ListView requests only the rows currently visible on screen.
struct EntryModel {
    archive: Arc<ArchiveData>,
    directory: String,
    notify: ModelNotify,
}
impl Model for EntryModel {
    type Data = ArchiveEntry;
    fn row_count(&self) -> usize {
        self.archive
            .directories
            .get(&self.directory)
            .map_or(0, Vec::len)
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
    fn row_data(&self, row: usize) -> Option<ArchiveEntry> {
        self.archive
            .directories
            .get(&self.directory)?
            .get(row)
            .map(|entry| ArchiveEntry {
                name: entry.name.as_str().into(),
                path: entry.path.as_str().into(),
                directory: entry.directory,
                icon: file_icon(&entry.name, entry.directory).into(),
                kind: if entry.directory {
                    "文件夹"
                } else {
                    "文件"
                }
                .into(),
                size: if entry.directory {
                    "—".into()
                } else {
                    size(entry.size).into()
                },
                compressed: if entry.directory {
                    "—".into()
                } else {
                    entry
                        .compressed
                        .map(size)
                        .unwrap_or_else(|| "—".into())
                        .into()
                },
            })
    }
}

struct TreeModel {
    archive: Arc<ArchiveData>,
    notify: ModelNotify,
}
impl Model for TreeModel {
    type Data = TreeEntry;
    fn row_count(&self) -> usize {
        self.archive.tree.len()
    }
    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
    fn row_data(&self, row: usize) -> Option<TreeEntry> {
        self.archive.tree.get(row).map(|path| TreeEntry {
            path: path.as_str().into(),
            name: path
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("")
                .into(),
            depth: path.matches('/').count().saturating_sub(1) as i32,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArchiveFormat {
    Zip,
    SevenZip,
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
        ArchiveFormat::SevenZip => {
            let reader = sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
            reader
                .archive()
                .files
                .iter()
                .filter(|entry| !(entry.name().is_empty() && entry.is_directory()))
                .map(|entry| {
                    let mode = (entry.windows_attributes >> 16) & 0xf000;
                    if entry.is_anti_item || (mode != 0 && mode != 0x8000 && mode != 0x4000) {
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
                if !kind.is_dir() && !kind.is_file() {
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
    Ok(ArchiveData {
        path: path.to_owned(),
        format,
        directories,
        tree: tree.into_iter().collect(),
    })
}

fn show_directory(ui: &AppWindow, archive: Arc<ArchiveData>, directory: &str) {
    if !archive.directories.contains_key(directory) {
        return;
    }
    ui.set_selected_entry("".into());
    ui.set_current_directory(directory.into());
    ui.set_entries(ModelRc::new(EntryModel {
        archive,
        directory: directory.to_owned(),
        notify: ModelNotify::default(),
    }));
}

fn show_archive(ui: &AppWindow, archive: Arc<ArchiveData>, preserve_directory: bool) {
    ui.set_archive_name(
        archive
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
            .into(),
    );
    let directory = if preserve_directory {
        ui.get_current_directory().to_string()
    } else {
        String::new()
    };
    ui.set_tree(ModelRc::new(TreeModel {
        archive: archive.clone(),
        notify: ModelNotify::default(),
    }));
    ui.set_has_archive(true);
    ui.set_can_add(archive.format == ArchiveFormat::Zip);
    if archive.directories.contains_key(&directory) {
        show_directory(ui, archive, &directory);
    } else {
        show_directory(ui, archive, "");
    }
}

fn run_job(
    ui: &AppWindow,
    current: Arc<Mutex<Option<Arc<ArchiveData>>>>,
    progress: &'static str,
    success: &'static str,
    job: impl FnOnce() -> Result<Option<ArchiveData>> + Send + 'static,
) {
    ui.set_busy(true);
    ui.set_status(progress.into());
    let weak = ui.as_weak();
    thread::spawn(move || {
        let result = job();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_busy(false);
            let result = result.map(|data| {
                if let Some(data) = data {
                    let mut current = current.lock().unwrap();
                    let preserve = current.as_ref().is_some_and(|old| old.path == data.path);
                    let archive = Arc::new(data);
                    *current = Some(archive.clone());
                    show_archive(&ui, archive, preserve);
                }
            });
            report(&ui, result, success);
        });
    });
}

fn report(ui: &AppWindow, result: Result<()>, success: &str) {
    match result {
        Ok(()) => ui.set_status(success.into()),
        Err(error) => {
            ui.set_status("操作失败".into());
            rfd::MessageDialog::new()
                .set_title("操作失败")
                .set_description(format!("{error:#}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
        }
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
        writer.start_file(
            format!("{directory}{}", file.file_name().unwrap().to_str().unwrap()),
            options,
        )?;
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
    let name = Path::new(selection).file_name().context("文件名无效")?;
    let directory = tempfile::Builder::new().prefix("sugar-open-").tempdir()?;
    extract_selection(archive, directory.path(), Some(selection))?;
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
        ArchiveFormat::SevenZip => {
            let mut reader =
                sevenz_rust2::ArchiveReader::open(path, sevenz_rust2::Password::empty())?;
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
                    || (!expected.directory && !kind.is_file())
                {
                    bail!("压缩包在解压过程中发生变化");
                }
                if let Some(target) = &targets[index] {
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

fn prepare_extract_dialog(ui: &AppWindow, path: &Path, item: &str) {
    ui.set_extract_item_path(item.into());
    ui.set_extract_destination(
        path.parent()
            .unwrap_or_else(|| Path::new("."))
            .to_string_lossy()
            .to_string()
            .into(),
    );
    ui.set_extra_folder_name(archive_folder_name(path).into());
    ui.set_create_extra_folder(false);
    ui.set_extract_dialog_open(true);
}

fn prepare_destination(destination: &Path, extra: Option<&str>) -> Result<PathBuf> {
    if !destination.is_dir() {
        bail!("解压位置不是已有文件夹：{}", destination.display());
    }
    let Some(name) = extra else {
        return Ok(destination.to_owned());
    };
    let mut components = Path::new(name).components();
    if name.contains('\\')
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        bail!("额外文件夹名称无效");
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

fn main() -> Result<()> {
    let ui = AppWindow::new()?;
    let current = Arc::new(Mutex::new(None::<Arc<ArchiveData>>));
    let temporary_files = Arc::new(Mutex::new(Vec::<tempfile::TempDir>::new()));
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_navigate(move |directory| {
            if let Some(archive) = current.lock().unwrap().clone() {
                show_directory(&weak.unwrap(), archive, directory.as_str());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_navigate_parent(move || {
            let ui = weak.unwrap();
            let directory = ui.get_current_directory().to_string();
            let parent = directory
                .trim_end_matches('/')
                .rsplit_once('/')
                .map_or("", |(parent, _)| parent);
            let parent = if parent.is_empty() {
                String::new()
            } else {
                format!("{parent}/")
            };
            if let Some(archive) = current.lock().unwrap().clone() {
                show_directory(&ui, archive, &parent);
            }
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_open_archive(move || {
            let ui = weak.unwrap();
            if ui.get_busy() {
                return;
            }
            let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "支持的压缩包",
                    &[
                        "zip", "7z", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz",
                    ],
                )
                .pick_file()
            else {
                return;
            };
            run_job(
                &ui,
                current.clone(),
                "正在打开压缩包…",
                "压缩包已打开",
                move || read_archive(&path).map(Some),
            );
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_create_archive(move || {
            let ui = weak.unwrap();
            if ui.get_busy() {
                return;
            }
            let Some(mut path) = rfd::FileDialog::new()
                .add_filter("ZIP 压缩包", &["zip"])
                .set_file_name("新建压缩包.zip")
                .save_file()
            else {
                return;
            };
            if path.extension().is_none() {
                path.set_extension("zip");
            }
            run_job(
                &ui,
                current.clone(),
                "正在创建压缩包…",
                "压缩包已创建",
                move || {
                    create_archive(&path)?;
                    read_archive(&path).map(Some)
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_add_files(move || {
            let ui = weak.unwrap();
            if ui.get_busy() {
                return;
            }
            let Some(archive) = current.lock().unwrap().clone() else {
                return;
            };
            if archive.format != ArchiveFormat::Zip {
                return;
            }
            let path = archive.path.clone();
            let directory = ui.get_current_directory().to_string();
            let Some(files) = rfd::FileDialog::new().pick_files() else {
                return;
            };
            run_job(
                &ui,
                current.clone(),
                "正在添加文件…",
                "文件已添加",
                move || {
                    add_files(&path, &directory, &files)?;
                    read_archive(&path).map(Some)
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_extract_archive(move || {
            let ui = weak.unwrap();
            if ui.get_busy() {
                return;
            }
            if let Some(archive) = current.lock().unwrap().clone() {
                prepare_extract_dialog(&ui, &archive.path, "");
            }
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        let temporary_files = temporary_files.clone();
        ui.on_open_item(move |item| {
            let ui = weak.unwrap();
            if ui.get_busy() {
                return;
            }
            let Some(archive) = current.lock().unwrap().clone() else {
                return;
            };
            let item = item.to_string();
            let directory = ui.get_current_directory().to_string();
            if !archive.directories.get(&directory).is_some_and(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.path == item && !entry.directory)
            }) {
                return;
            }
            let path = archive.path.clone();
            let temporary_files = temporary_files.clone();
            ui.set_busy(true);
            ui.set_status("正在打开文件…".into());
            let weak = ui.as_weak();
            thread::spawn(move || {
                let result = (|| {
                    let (directory, file) = extract_temporary_file(&path, &item)?;
                    open_with_default_app(&file)?;
                    temporary_files.lock().unwrap().push(directory);
                    Ok(())
                })();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_busy(false);
                    report(&ui, result, "文件已打开");
                });
            });
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_extract_item(move |item| {
            let ui = weak.unwrap();
            if item.is_empty() || item != ui.get_selected_entry() || ui.get_busy() {
                return;
            }
            if let Some(archive) = current.lock().unwrap().clone() {
                prepare_extract_dialog(&ui, &archive.path, item.as_str());
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_choose_destination(move || {
            let ui = weak.unwrap();
            let mut dialog = rfd::FileDialog::new();
            let previous = ui.get_extract_destination().to_string();
            if Path::new(&previous).is_dir() {
                dialog = dialog.set_directory(previous);
            }
            if let Some(path) = dialog.pick_folder() {
                ui.set_extract_destination(path.to_string_lossy().to_string().into());
            }
        });
    }
    {
        let weak = ui.as_weak();
        let current = current.clone();
        ui.on_confirm_extract(move || {
            let ui = weak.unwrap();
            if ui.get_busy() || !ui.get_extract_dialog_open() {
                return;
            }
            let Some(archive) = current.lock().unwrap().clone() else {
                return;
            };
            let destination = PathBuf::from(ui.get_extract_destination().as_str());
            if destination.as_os_str().is_empty() {
                return;
            }
            let item = ui.get_extract_item_path().to_string();
            let extra = ui.get_create_extra_folder();
            let folder = ui.get_extra_folder_name().to_string();
            let path = archive.path.clone();
            ui.set_extract_dialog_open(false);
            ui.set_busy(true);
            ui.set_status("正在解压文件…".into());
            ui.set_extract_progress(0.0);
            ui.set_extract_progress_text("正在检查压缩包…".into());
            ui.set_extract_remaining("预计剩余时间：计算中…".into());
            ui.set_progress_dialog_open(true);
            let weak = ui.as_weak();
            thread::spawn(move || {
                let result = (|| {
                    let destination =
                        prepare_destination(&destination, extra.then_some(folder.as_str()))?;
                    let started = Instant::now();
                    let mut last_update = None::<Instant>;
                    let progress_ui = weak.clone();
                    extract_selection_with_progress(
                        &path,
                        &destination,
                        (!item.is_empty()).then_some(item.as_str()),
                        &mut |completed, total| {
                            let now = Instant::now();
                            if last_update.is_some_and(|last| {
                                now.duration_since(last) < Duration::from_millis(150)
                            }) {
                                return;
                            }
                            last_update = Some(now);
                            let fraction = if total == 0 {
                                0.0
                            } else {
                                (completed as f64 / total as f64).min(0.99) as f32
                            };
                            let label = if total == 0 {
                                "正在处理空文件和文件夹…".to_owned()
                            } else {
                                format!(
                                    "{} / {}（{}%）",
                                    size(completed.min(total)),
                                    size(total),
                                    (fraction * 100.0) as u32
                                )
                            };
                            let remaining = remaining_time(completed, total, started.elapsed());
                            let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                                ui.set_extract_progress(fraction);
                                ui.set_extract_progress_text(label.into());
                                ui.set_extract_remaining(remaining.into());
                            });
                        },
                    )
                })();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_progress_dialog_open(false);
                    ui.set_busy(false);
                    report(&ui, result, "解压完成");
                });
            });
        });
    }
    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(prepare_destination(dir.path(), Some("../escape")).is_err());
        assert!(prepare_destination(dir.path(), Some("..")).is_err());
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
    fn read_many_entries_and_access_virtualized_rows() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("many.zip");
        let mut writer = ZipWriter::new(File::create(&path)?);
        for index in 0..10_000 {
            writer.start_file(format!("file-{index}.txt"), SimpleFileOptions::default())?;
        }
        writer.finish()?;
        let data = read_archive(&path)?;
        let model = EntryModel {
            archive: Arc::new(data),
            directory: String::new(),
            notify: ModelNotify::default(),
        };
        assert_eq!(model.row_count(), 10_000);
        assert_eq!(
            model.row_data(9_999).unwrap().name.as_str(),
            "file-9999.txt"
        );
        assert!(model.row_data(10_000).is_none());
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
