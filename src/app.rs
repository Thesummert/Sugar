use super::*;
use serde::Serialize;
use tauri::{Emitter, Manager};

#[derive(Default)]
struct State {
    current: Mutex<Option<Arc<ArchiveData>>>,
    temporary: Mutex<Vec<tempfile::TempDir>>,
    prepared: Mutex<Option<PreparedDrag>>,
    pending_open: Mutex<VecDeque<PathBuf>>,
}
#[derive(Serialize)]
struct View {
    name: String,
    path: String,
    staging: bool,
    can_add: bool,
    tree: Vec<Tree>,
    entries: Vec<Row>,
    directory: String,
    folder: String,
    parent: String,
}
#[derive(Serialize)]
struct Tree {
    name: String,
    path: String,
    depth: usize,
}
#[derive(Serialize)]
struct Row {
    name: String,
    path: String,
    directory: bool,
    icon: String,
    kind: String,
    size: String,
    compressed: String,
}
fn view(a: &ArchiveData, directory: &str) -> View {
    let source = a.staged_sources.as_ref().and_then(|s| s.first());
    let display_path = source.map(PathBuf::as_path).unwrap_or(&a.path);
    let staged_name = a.staged_sources.as_ref().map(|s| {
        if s.len() == 1 {
            display_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        } else {
            "新建压缩包".into()
        }
    });
    let directory = if a.directories.contains_key(directory) {
        directory
    } else {
        ""
    };
    View {
        name: if a.staged_sources.is_some() {
            "待压缩文件".into()
        } else {
            a.path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        },
        path: a.path.to_string_lossy().into_owned(),
        staging: a.staged_sources.is_some(),
        can_add: a.staged_sources.is_none() && a.format == ArchiveFormat::Zip,
        tree: a
            .tree
            .iter()
            .map(|p| Tree {
                name: p
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .into(),
                path: p.clone(),
                depth: p.matches('/').count().saturating_sub(1),
            })
            .collect(),
        entries: a
            .directories
            .get(directory)
            .into_iter()
            .flatten()
            .map(|e| Row {
                name: e.name.clone(),
                path: e.path.clone(),
                directory: e.directory,
                icon: file_icon(&e.name, e.directory).into(),
                kind: if e.directory { "文件夹" } else { "文件" }.into(),
                size: if e.directory {
                    "—".into()
                } else {
                    size(e.size)
                },
                compressed: if e.directory {
                    "—".into()
                } else {
                    e.compressed.map(size).unwrap_or_else(|| "—".into())
                },
            })
            .collect(),
        directory: directory.into(),
        folder: staged_name.unwrap_or_else(|| archive_folder_name(&a.path)),
        parent: display_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_string_lossy()
            .into_owned(),
    }
}
fn current(state: &State) -> Result<Arc<ArchiveData>> {
    state
        .current
        .lock()
        .unwrap()
        .clone()
        .context("尚未打开压缩包")
}
fn install(state: &State, data: ArchiveData, directory: &str) -> View {
    let a = Arc::new(data);
    let v = view(&a, directory);
    *state.current.lock().unwrap() = Some(a);
    *state.prepared.lock().unwrap() = None;
    v
}
fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}
async fn job<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> std::result::Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
        .map_err(err)
}
#[tauri::command]
async fn startup(app: tauri::AppHandle) -> std::result::Result<Option<View>, String> {
    job(move || {
        let args: Vec<_> = std::env::args_os().collect();
        if args.get(1).is_some_and(|x| x == "--pudding-stage") {
            let paths = args[2..].iter().map(PathBuf::from).collect();
            Ok(Some(install(
                &app.state::<State>(),
                stage_archive_inputs(paths)?,
                "",
            )))
        } else if let Some(path) = args.get(1) {
            Ok(Some(install(
                &app.state::<State>(),
                read_archive(Path::new(path))?,
                "",
            )))
        } else {
            Ok(None)
        }
    })
    .await
}
#[tauri::command]
fn take_open(state: tauri::State<'_, State>) -> Option<String> {
    state
        .pending_open
        .lock()
        .unwrap()
        .pop_front()
        .map(|p| p.to_string_lossy().into_owned())
}
#[tauri::command]
async fn open_archive(
    app: tauri::AppHandle,
    path: Option<String>,
) -> std::result::Result<Option<View>, String> {
    job(move || {
        let path = match path {
            Some(p) => Some(PathBuf::from(p)),
            None => rfd::FileDialog::new()
                .add_filter(
                    "支持的压缩包",
                    &[
                        "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "tbz", "tbz2", "xz", "txz",
                    ],
                )
                .pick_file(),
        };
        path.map(|p| read_archive(&p).map(|a| install(&app.state::<State>(), a, "")))
            .transpose()
    })
    .await
}
#[tauri::command]
async fn create_empty(app: tauri::AppHandle) -> std::result::Result<Option<View>, String> {
    job(move || {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("ZIP 压缩包", &["zip"])
            .set_file_name("新建压缩包.zip")
            .save_file()
        else {
            return Ok(None);
        };
        if path.extension().is_none() {
            path.set_extension("zip");
        }
        create_archive(&path)?;
        Ok(Some(install(
            &app.state::<State>(),
            read_archive(&path)?,
            "",
        )))
    })
    .await
}
#[tauri::command]
async fn stage(app: tauri::AppHandle, paths: Vec<String>) -> std::result::Result<View, String> {
    job(move || {
        let state = app.state::<State>();
        let mut sources = state
            .current
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|a| a.staged_sources.clone())
            .unwrap_or_default();
        sources.extend(paths.into_iter().map(PathBuf::from));
        Ok(install(&state, stage_archive_inputs(sources)?, ""))
    })
    .await
}
#[tauri::command]
fn navigate(
    state: tauri::State<'_, State>,
    directory: String,
) -> std::result::Result<View, String> {
    let archive = current(&state).map_err(err)?;
    Ok(view(&archive, &directory))
}
#[tauri::command]
async fn choose_folder() -> std::result::Result<Option<String>, String> {
    job(|| {
        Ok(rfd::FileDialog::new()
            .pick_folder()
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
}
#[tauri::command]
async fn add(
    app: tauri::AppHandle,
    directory: String,
) -> std::result::Result<Option<View>, String> {
    job(move || {
        let state = app.state::<State>();
        let a = current(&state)?;
        if a.staged_sources.is_some()
            || a.format != ArchiveFormat::Zip
            || !a.directories.contains_key(&directory)
        {
            bail!("无法向此目录添加文件")
        }
        let Some(files) = rfd::FileDialog::new().pick_files() else {
            return Ok(None);
        };
        add_files(&a.path, &directory, &files)?;
        Ok(Some(install(&state, read_archive(&a.path)?, &directory)))
    })
    .await
}
#[tauri::command]
async fn compress(
    app: tauri::AppHandle,
    destination: String,
    name: String,
    format: String,
    level: i32,
) -> std::result::Result<View, String> {
    job(move || {
        let state = app.state::<State>();
        let a = current(&state)?;
        let sources = a.staged_sources.as_ref().context("没有待压缩文件")?;
        let path = compression_target(Path::new(&destination), &name, &format)?;
        create_archive_from_files(&path, sources, level)?;
        Ok(install(&state, read_archive(&path)?, ""))
    })
    .await
}
fn checked_item(a: &ArchiveData, item: &str) -> Result<()> {
    if a.staged_sources.is_some() || !a.directories.values().flatten().any(|e| e.path == item) {
        bail!("无效的压缩包条目")
    }
    Ok(())
}
#[derive(Clone, Serialize)]
struct Progress {
    fraction: f64,
    text: String,
    remaining: String,
}
#[tauri::command]
async fn extract(
    app: tauri::AppHandle,
    destination: String,
    item: String,
    extra: Option<String>,
) -> std::result::Result<(), String> {
    job(move || {
        let a = current(&app.state::<State>())?;
        if !item.is_empty() {
            checked_item(&a, &item)?;
        }
        if a.staged_sources.is_some() {
            bail!("尚未创建压缩包")
        }
        let dest = prepare_destination(Path::new(&destination), extra.as_deref())?;
        let started = Instant::now();
        let mut last = None::<Instant>;
        extract_selection_with_progress(
            &a.path,
            &dest,
            (!item.is_empty()).then_some(item.as_str()),
            &mut |done, total| {
                let now = Instant::now();
                if last.is_some_and(|t| now.duration_since(t) < Duration::from_millis(150)) {
                    return;
                }
                last = Some(now);
                let fraction = if total == 0 {
                    0.0
                } else {
                    (done as f64 / total as f64).min(0.99)
                };
                let _ = app.emit(
                    "extract-progress",
                    Progress {
                        fraction,
                        text: if total == 0 {
                            "正在处理空文件和文件夹…".into()
                        } else {
                            format!(
                                "{} / {}（{}%）",
                                size(done.min(total)),
                                size(total),
                                (fraction * 100.0) as u32
                            )
                        },
                        remaining: remaining_time(done, total, started.elapsed()),
                    },
                );
            },
        )
    })
    .await
}
#[tauri::command]
async fn open_item(app: tauri::AppHandle, item: String) -> std::result::Result<(), String> {
    job(move || {
        let state = app.state::<State>();
        let a = current(&state)?;
        checked_item(&a, &item)?;
        let (dir, path) = extract_temporary_file(&a.path, &item)?;
        open_with_default_app(&path)?;
        state.temporary.lock().unwrap().push(dir);
        Ok(())
    })
    .await
}
#[tauri::command]
async fn prepare_drag(app: tauri::AppHandle, item: String) -> std::result::Result<String, String> {
    job(move || {
        let state = app.state::<State>();
        let a = current(&state)?;
        checked_item(&a, &item)?;
        let version = ArchiveVersion::read(&a.path)?;
        if let Some(p) = state
            .prepared
            .lock()
            .unwrap()
            .as_ref()
            .filter(|p| p.matches(&a, &item, &version))
        {
            return Ok(p.path.to_string_lossy().into_owned());
        }
        let (dir, path) = extract_temporary_selection(&a.path, &item, &mut |_, _| {})?;
        if ArchiveVersion::read(&a.path)? != version {
            bail!("压缩包在解压过程中发生变化，请重新拖动")
        };
        state.temporary.lock().unwrap().push(dir);
        *state.prepared.lock().unwrap() = Some(PreparedDrag {
            archive: a,
            item,
            version,
            path: path.clone(),
        });
        Ok(path.to_string_lossy().into_owned())
    })
    .await
}
pub fn run() -> Result<()> {
    tauri::Builder::default()
        .manage(State::default())
        .plugin(tauri_plugin_drag::init())
        .invoke_handler(tauri::generate_handler![
            startup,
            take_open,
            open_archive,
            create_empty,
            stage,
            navigate,
            choose_folder,
            add,
            compress,
            extract,
            open_item,
            prepare_drag
        ])
        .build(tauri::generate_context!())?
        .run(|_app, _event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = _event {
                let state = _app.state::<State>();
                let mut pending = state.pending_open.lock().unwrap();
                pending.extend(urls.into_iter().filter_map(|u| u.to_file_path().ok()));
                drop(pending);
                let _ = _app.emit("open-files", ());
            }
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staged_view_suggests_source_location_and_validates_items() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("one.txt");
        fs::write(&source, "data")?;
        let data = stage_archive_inputs(vec![source])?;
        let result = view(&data, "");
        assert_eq!(result.folder, "one.txt");
        assert_eq!(Path::new(&result.parent), dir.path());
        assert!(result.staging);
        assert!(checked_item(&data, "one.txt").is_err());
        Ok(())
    }
}
