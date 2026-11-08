use anyhow::{bail, Context, Result};
use slint::{ComponentHandle, Model, ModelNotify, ModelRc, ModelTracker};
use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
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
    compressed: u64,
}

struct ArchiveData {
    path: PathBuf,
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
                    size(entry.compressed).into()
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

fn read_archive(path: &Path) -> Result<ArchiveData> {
    let mut archive = ZipArchive::new(File::open(path)?).context("无法读取 ZIP 压缩包")?;
    let mut directories: HashMap<String, Vec<EntryData>> = HashMap::new();
    let mut tree = BTreeSet::new();
    directories.entry(String::new()).or_default();
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let name = decoded_name(file.name_raw(), file.name());
        let name = name.as_ref();
        // Unsafe or ambiguous names remain inaccessible in the browser, and extraction
        // independently rejects unsafe paths.
        if name.starts_with('/') || name.split('/').any(|part| part == "." || part == "..") {
            continue;
        }
        let parts: Vec<_> = name.split('/').filter(|part| !part.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let directory_count = if file.is_dir() {
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
                        compressed: 0,
                    });
                directories.entry(child.clone()).or_default();
            }
            parent = child;
        }
        if !file.is_dir() {
            directories.entry(parent).or_default().push(EntryData {
                name: parts.last().unwrap().to_string(),
                path: name.to_owned(),
                directory: false,
                size: file.size(),
                compressed: file.compressed_size(),
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
        directories,
        tree: tree.into_iter().collect(),
    })
}

fn show_directory(ui: &AppWindow, archive: Arc<ArchiveData>, directory: &str) {
    if !archive.directories.contains_key(directory) {
        return;
    }
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

fn extract_archive(path: &Path, destination: &Path) -> Result<()> {
    let mut zip = ZipArchive::new(File::open(path)?)?;
    let mut targets = Vec::with_capacity(zip.len());
    let mut file_paths = HashSet::new();
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if entry.is_symlink() {
            bail!("暂不支持解压符号链接：{}", entry.name());
        }
        let name = decoded_name(entry.name_raw(), entry.name());
        let relative = Path::new(name.as_ref());
        if name.contains('\\')
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || relative.as_os_str().is_empty()
        {
            bail!("压缩包包含不安全的路径：{name}");
        }
        let target = destination.join(relative);
        if !entry.is_dir() && !file_paths.insert(target.clone()) {
            bail!("压缩包包含同名文件：{name}");
        }
        if let Ok(metadata) = fs::symlink_metadata(&target) {
            if !entry.is_dir() || !metadata.is_dir() || metadata.file_type().is_symlink() {
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
        targets.push((target, entry.is_dir()));
    }
    for (target, directory) in &targets {
        if *directory && file_paths.contains(target) {
            bail!("压缩包中的文件与目录路径冲突：{}", target.display());
        }
        if target
            .ancestors()
            .skip(1)
            .take_while(|p| *p != destination)
            .any(|p| file_paths.contains(p))
        {
            bail!("压缩包中的文件与目录路径冲突：{}", target.display());
        }
    }
    for (i, (target, directory)) in targets.into_iter().enumerate() {
        if directory {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .with_context(|| format!("无法创建 {}", target.display()))?;
            io::copy(&mut zip.by_index(i)?, &mut output).context("解压失败")?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let ui = AppWindow::new()?;
    let current = Arc::new(Mutex::new(None::<Arc<ArchiveData>>));
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
                .add_filter("ZIP 压缩包", &["zip"])
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
            let Some(archive) = current.lock().unwrap().clone() else {
                return;
            };
            let path = archive.path.clone();
            let Some(destination) = rfd::FileDialog::new().pick_folder() else {
                return;
            };
            run_job(
                &ui,
                current.clone(),
                "正在解压文件…",
                "解压完成",
                move || {
                    extract_archive(&path, &destination)?;
                    Ok(None)
                },
            );
        });
    }
    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
