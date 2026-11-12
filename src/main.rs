use axum::{
    Form, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use chrono::{DateTime, Local};
use minijinja::{AutoEscape, Environment};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    fs, io,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::UNIX_EPOCH,
};
use tokio::{io::AsyncWriteExt, process::Command};
use tower_http::services::ServeDir;

#[derive(Clone)]
struct AppState {
    root: PathBuf,
    templates: PathBuf,
}

struct StageDir(PathBuf);

impl Drop for StageDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn new_stage_dir(root: &Path) -> io::Result<StageDir> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    for _ in 0..100 {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(".staging-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(StageDir(path)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an upload staging directory",
    ))
}

#[derive(Deserialize, Default)]
struct PathQuery {
    path: Option<String>,
    upload_error: Option<String>,
}

#[derive(Deserialize)]
struct CreateFolder {
    folder_name: String,
    #[serde(default)]
    path: String,
}
#[derive(Deserialize)]
struct RenameForm {
    old_name: String,
    new_name: String,
    #[serde(default)]
    path: String,
}
#[derive(Deserialize)]
struct ItemForm {
    item_name: String,
    #[serde(default)]
    path: String,
    dest: String,
}
#[derive(Deserialize)]
struct PasteForm {
    item_name: String,
    action: String,
    #[serde(default)]
    path: String,
    dest: String,
}
#[derive(Deserialize)]
struct TrashForm {
    trash_name: String,
}

fn safe_rel(s: &str) -> Option<PathBuf> {
    let p = Path::new(s);
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return if s.is_empty() {
            Some(PathBuf::new())
        } else {
            None
        };
    }
    Some(p.to_path_buf())
}
fn path_under(root: &Path, rel: &str) -> Option<PathBuf> {
    let path = safe_rel(rel)?;
    if path
        .components()
        .any(|part| part.as_os_str().to_string_lossy().starts_with(".staging-"))
    {
        return None;
    }
    Some(root.join(path))
}
fn redirect(path: &str) -> Redirect {
    let encoded = path
        .split('/')
        .map(|s| urlencoding::encode(s).into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Redirect::to(&format!("/files?path={encoded}"))
}
fn render(state: &AppState, name: &str, context: Value) -> Response {
    let mut env = Environment::new();
    env.set_auto_escape_callback(|template| {
        if template.ends_with(".html") {
            AutoEscape::Html
        } else {
            AutoEscape::None
        }
    });
    let source = match fs::read_to_string(state.templates.join(name)) {
        Ok(v) => v,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    if env.add_template_owned(name.to_string(), source).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    match env.get_template(name).and_then(|t| t.render(context)) {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            eprintln!("Template rendering failed for {name}: {err}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
fn human_size(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.2} KB", n as f64 / 1024.)
    } else {
        format!("{:.2} MB", n as f64 / (1024. * 1024.))
    }
}
fn dir_size(p: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(p) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                p.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}
fn unique_path(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("item");
    let ext = p
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| format!(".{x}"))
        .unwrap_or_default();
    for i in 1.. {
        let c = p.with_file_name(format!("{stem}_{i}{ext}"));
        if !c.exists() {
            return c;
        }
    }
    unreachable!()
}
fn ensure_trash(state: &AppState) -> io::Result<PathBuf> {
    let d = state.root.join(".trash");
    fs::create_dir_all(&d)?;
    let m = d.join("trash.json");
    if !m.exists() {
        fs::write(&m, "{}")?;
    }
    Ok(m)
}
fn metadata(state: &AppState) -> Map<String, Value> {
    let Ok(p) = ensure_trash(state) else {
        return Map::new();
    };
    fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}
fn save_metadata(state: &AppState, data: &Map<String, Value>) {
    if let Ok(p) = ensure_trash(state) {
        let _ = fs::write(p, serde_json::to_vec_pretty(data).unwrap_or_default());
    }
}
fn files_page(state: &AppState, path: &str, upload_error: &str) -> Response {
    let Some(mut current) = path_under(&state.root, path) else {
        return redirect("").into_response();
    };
    if !current.is_dir() {
        current = state.root.clone();
    }
    let actual_path = current.strip_prefix(&state.root).unwrap_or(Path::new(""));
    let current_path = actual_path.to_string_lossy().replace('\\', "/");
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(&current) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".trash" || name.starts_with(".staging-") {
                continue;
            }
            let md = match e.metadata() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let lower_name = name.to_lowercase();
            let is_preview = [".png", ".jpg", ".jpeg", ".gif", ".webp", ".pdf"]
                .iter()
                .any(|ext| lower_name.ends_with(ext));
            let has_preview = is_preview
                || [".mp4", ".webm"]
                    .iter()
                    .any(|ext| lower_name.ends_with(ext));
            let modified = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| {
                    DateTime::<Local>::from(UNIX_EPOCH + d)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            files.push(json!({"name":name,"size":if md.is_file(){human_size(md.len())}else{"-".into()},"date":modified,"is_dir":md.is_dir(),"is_preview":is_preview,"has_preview":has_preview}));
        }
    }
    files.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });
    let parent = Path::new(&current_path)
        .parent()
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .replace('\\', "/");
    let parent = urlencoding::encode(&parent).into_owned();
    render(
        state,
        "files.html",
        json!({"files":files,"current_path":current_path,"parent":parent,"upload_error":upload_error}),
    )
}
async fn home(State(s): State<Arc<AppState>>) -> Response {
    render(
        &s,
        "index.html",
        json!({"scanner_installed":Command::new("clamscan").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().await.is_ok()}),
    )
}
async fn files(State(s): State<Arc<AppState>>, Query(q): Query<PathQuery>) -> Response {
    files_page(
        &s,
        q.path.as_deref().unwrap_or(""),
        q.upload_error.as_deref().unwrap_or(""),
    )
}

async fn scan_uploads(paths: &[PathBuf]) -> Result<std::process::Output, &'static str> {
    let daemon_scan = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        Command::new("clamdscan")
            .args(["--no-summary", "--fdpass"])
            .args(paths)
            .output(),
    )
    .await;

    match daemon_scan {
        Err(_) => return Err("scan_failed"),
        Ok(Ok(output)) if matches!(output.status.code(), Some(0 | 1)) => return Ok(output),
        // No running clamd, or another daemon error: fall back to clamscan.
        _ => {}
    }

    let mut scanner = Command::new("clamscan");
    scanner.args([
        "--no-summary",
        "--max-filesize=110M",
        "--max-scansize=200M",
        "--alert-encrypted=yes",
        "--alert-exceeds-max=yes",
    ]);
    scanner.args(paths);
    match tokio::time::timeout(std::time::Duration::from_secs(300), scanner.output()).await {
        Err(_) => Err("scan_failed"),
        Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => Err("scanner_unavailable"),
        Ok(Err(_)) => Err("scan_failed"),
        Ok(Ok(output)) => Ok(output),
    }
}

async fn upload(State(s): State<Arc<AppState>>, mut multipart: Multipart) -> Response {
    let mut path = String::new();
    let mut staged: Vec<(PathBuf, String)> = Vec::new();
    // Keep staging on the uploads filesystem so publishing a clean file is a
    // cheap rename instead of another full read and write.
    let temp = match new_stage_dir(&s.root) {
        Ok(dir) => dir,
        Err(_) => return upload_error("", "scan_failed"),
    };
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(_) => {
                return redirect(&path).into_response();
            }
        };
        if field.name() == Some("path") {
            path = field.text().await.unwrap_or_default();
            continue;
        }
        if field.name() != Some("files") {
            continue;
        }
        let name = field
            .file_name()
            .unwrap_or("file")
            .replace('\\', "/")
            .split('/')
            .last()
            .unwrap_or("file")
            .to_string();
        if name.is_empty() || name == "." || name == ".." {
            return upload_error(&path, "scan_failed");
        }
        let dest = temp.0.join(staged.len().to_string());
        let Ok(mut out) = tokio::fs::File::create(&dest).await else {
            return upload_error(&path, "scan_failed");
        };
        let mut field = field;
        let mut size = 0u64;
        loop {
            match field.chunk().await {
                Ok(Some(bytes)) => {
                    size += bytes.len() as u64;
                    if size > 100 * 1024 * 1024 {
                        return upload_error(&path, "too_large");
                    }
                    if out.write_all(&bytes).await.is_err() {
                        return upload_error(&path, "scan_failed");
                    }
                }
                Ok(None) => break,
                Err(_) => return upload_error(&path, "scan_failed"),
            }
        }
        staged.push((dest, name));
    }
    let scan_paths: Vec<PathBuf> = staged.iter().map(|(p, _)| p.clone()).collect();
    let scan = match scan_uploads(&scan_paths).await {
        Ok(output) => output,
        Err(reason) => return upload_error(&path, reason),
    };
    if !scan.status.success() {
        return upload_error(
            &path,
            if scan.status.code() == Some(1) {
                "scan_blocked"
            } else {
                "scan_failed"
            },
        );
    }
    let Some(dir) = path_under(&s.root, &path) else {
        return upload_error("", "scan_failed");
    };
    if fs::create_dir_all(&dir).is_err() {
        return upload_error(&path, "scan_failed");
    }
    for (src, name) in staged {
        if fs::rename(src, dir.join(name)).is_err() {
            return upload_error(&path, "scan_failed");
        }
    }
    redirect(&path).into_response()
}
fn upload_error(path: &str, error: &str) -> Response {
    Redirect::to(&format!(
        "/files?path={}&upload_error={}",
        urlencoding::encode(path),
        error
    ))
    .into_response()
}
async fn create_folder(State(s): State<Arc<AppState>>, Form(f): Form<CreateFolder>) -> Response {
    if let (Some(d), Some(n)) = (path_under(&s.root, &f.path), safe_rel(&f.folder_name)) {
        let _ = fs::create_dir_all(d.join(n));
    }
    redirect(&f.path).into_response()
}
async fn rename(State(s): State<Arc<AppState>>, Form(f): Form<RenameForm>) -> Response {
    if let (Some(d), Some(a), Some(b)) = (
        path_under(&s.root, &f.path),
        safe_rel(&f.old_name),
        safe_rel(&f.new_name),
    ) {
        let _ = fs::rename(d.join(a), d.join(b));
    }
    redirect(&f.path).into_response()
}
fn file_response(p: PathBuf, download: bool) -> Response {
    match fs::read(&p) {
        Ok(bytes) => {
            let mut r = Response::new(Body::from(bytes));
            if download {
                let n = p
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or("download")
                    .replace('"', "");
                r.headers_mut().insert(
                    header::CONTENT_DISPOSITION,
                    HeaderValue::from_str(&format!("attachment; filename=\"{n}\""))
                        .unwrap_or(HeaderValue::from_static("attachment")),
                );
            }
            r
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
async fn download(
    State(s): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    path_under(&s.root, &name)
        .map(|p| file_response(p, true))
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}
async fn preview(
    State(s): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    path_under(&s.root, &name)
        .map(|p| file_response(p, false))
        .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}
async fn delete(
    State(s): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    let Some(src) = path_under(&s.root, &name) else {
        return redirect("").into_response();
    };
    if !src.exists() {
        return redirect("").into_response();
    }
    let trash = s.root.join(".trash");
    let _ = fs::create_dir_all(&trash);
    let base = src
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let dest = unique_path(&trash.join(&base));
    let trash_name = dest
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    if fs::rename(&src, &dest).is_ok() {
        let mut m = metadata(&s);
        m.insert(trash_name,json!({"original":name,"deleted_at":Local::now().format("%Y-%m-%d %H:%M").to_string(),"is_dir":dest.is_dir()}));
        save_metadata(&s, &m);
    }
    redirect(
        Path::new(&name)
            .parent()
            .unwrap_or(Path::new(""))
            .to_str()
            .unwrap_or(""),
    )
    .into_response()
}
async fn folders(State(s): State<Arc<AppState>>) -> Response {
    fn visit(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(es) = fs::read_dir(dir) else { return };
        for e in es.flatten() {
            let p = e.path();
            if p.is_dir() {
                if e.file_name().to_string_lossy().starts_with(".staging-") {
                    continue;
                }
                if let Ok(r) = p.strip_prefix(root) {
                    out.push(r.to_string_lossy().replace('\\', "/"));
                    visit(root, &p, out)
                }
            }
        }
    }
    let mut v = vec![String::new()];
    visit(&s.root, &s.root, &mut v);
    v.sort();
    axum::Json(v).into_response()
}
fn copy_move(s: &AppState, f: &ItemForm, do_move: bool) -> Response {
    let Some(base) = path_under(&s.root, &f.path) else {
        return redirect(&f.path).into_response();
    };
    let Some(src_rel) = safe_rel(&f.item_name) else {
        return redirect(&f.path).into_response();
    };
    let Some(dst_rel) = safe_rel(&f.dest) else {
        return redirect(&f.path).into_response();
    };
    let src = base.join(src_rel);
    let dest = s.root.join(dst_rel);
    if !src.exists() || !dest.is_dir() || (src.is_dir() && dest.starts_with(&src)) {
        return redirect(&f.path).into_response();
    };
    let target = unique_path(&dest.join(src.file_name().unwrap_or_default()));
    let res = if do_move {
        fs::rename(src, target)
    } else if src.is_dir() {
        copy_dir(&src, &target)
    } else {
        fs::copy(src, target).map(|_| ())
    };
    let _ = res;
    redirect(&f.path).into_response()
}
fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let d = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &d)?
        } else {
            fs::copy(e.path(), d)?;
        }
    }
    Ok(())
}
async fn move_item(State(s): State<Arc<AppState>>, Form(f): Form<ItemForm>) -> Response {
    copy_move(&s, &f, true)
}
async fn copy_item(State(s): State<Arc<AppState>>, Form(f): Form<ItemForm>) -> Response {
    copy_move(&s, &f, false)
}
async fn paste_item(State(s): State<Arc<AppState>>, Form(f): Form<PasteForm>) -> Response {
    let i = ItemForm {
        item_name: f.item_name,
        path: f.path.clone(),
        dest: f.dest,
    };
    copy_move(&s, &i, f.action == "cut")
}
async fn trash(State(s): State<Arc<AppState>>) -> Response {
    let m = metadata(&s);
    let mut items = Vec::new();
    for (name, info) in m {
        let p = s.root.join(".trash").join(&name);
        if !p.exists() {
            continue;
        }
        let original = info["original"].as_str().unwrap_or("");
        let size = if info["is_dir"].as_bool().unwrap_or(false) {
            dir_size(&p)
        } else {
            p.metadata().map(|x| x.len()).unwrap_or(0)
        };
        items.push(json!({"trash_name":name,"name":Path::new(original).file_name().unwrap_or_default().to_string_lossy(),"original":original,"deleted_at":info["deleted_at"],"is_dir":info["is_dir"],"size":human_size(size)}));
    }
    items.sort_by(|a, b| {
        b["deleted_at"]
            .as_str()
            .unwrap_or("")
            .cmp(a["deleted_at"].as_str().unwrap_or(""))
    });
    render(&s, "trash.html", json!({"items":items}))
}
async fn restore(State(s): State<Arc<AppState>>, Form(f): Form<TrashForm>) -> Response {
    let mut m = metadata(&s);
    if let Some(info) = m.get(&f.trash_name) {
        let Some(rel) = safe_rel(info["original"].as_str().unwrap_or("")) else {
            return Redirect::to("/trash").into_response();
        };
        let p = s.root.join(".trash").join(&f.trash_name);
        let target = unique_path(&s.root.join(rel));
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::rename(p, target);
    }
    m.remove(&f.trash_name);
    save_metadata(&s, &m);
    Redirect::to("/trash").into_response()
}
async fn trash_delete(State(s): State<Arc<AppState>>, Form(f): Form<TrashForm>) -> Response {
    let mut m = metadata(&s);
    let Some(rel) = safe_rel(&f.trash_name) else {
        return Redirect::to("/trash").into_response();
    };
    remove(&s.root.join(".trash").join(rel));
    m.remove(&f.trash_name);
    save_metadata(&s, &m);
    Redirect::to("/trash").into_response()
}
fn remove(p: &Path) {
    if p.is_dir() {
        let _ = fs::remove_dir_all(p);
    } else {
        let _ = fs::remove_file(p);
    }
}
async fn trash_empty(State(s): State<Arc<AppState>>) -> Response {
    let mut m = metadata(&s);
    for n in m.keys() {
        if let Some(r) = safe_rel(n) {
            remove(&s.root.join(".trash").join(r));
        }
    }
    m.clear();
    save_metadata(&s, &m);
    Redirect::to("/trash").into_response()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::current_dir()?;
    let root = base.join("uploads");
    fs::create_dir_all(&root)?;
    let state = Arc::new(AppState {
        root,
        templates: base.join("app/templates"),
    });
    let app = Router::new()
        .route("/", get(home))
        .route("/files", get(files))
        .route("/upload", post(upload))
        .route("/create-folder", post(create_folder))
        .route("/rename", post(rename))
        .route("/download/{*filename}", get(download))
        .route("/preview/{*filename}", get(preview))
        .route("/delete/{*filename}", get(delete))
        .route("/folders", get(folders))
        .route("/move", post(move_item))
        .route("/copy", post(copy_item))
        .route("/paste", post(paste_item))
        .route("/trash", get(trash))
        .route("/trash/restore", post(restore))
        .route("/trash/delete", post(trash_delete))
        .route("/trash/empty", post(trash_empty))
        .nest_service("/static", ServeDir::new(base.join("app/static")))
        .layer(DefaultBodyLimit::max(110 * 1024 * 1024))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await?;
    println!("SecureShare running at http://0.0.0.0:8000");
    axum::serve(listener, app).await?;
    Ok(())
}
