mod browser;
mod cli;
mod logging;

use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, DefaultBodyLimit, Multipart, Path, Query, Request, State,
    },
    http::header::CONTENT_DISPOSITION,
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures_util::TryStreamExt;
use qrcode::{render::unicode, QrCode};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, IsTerminal},
    net::{Ipv4Addr, SocketAddr},
    path::{Path as FilePath, PathBuf},
    process::Command,
    sync::Arc,
    time::Instant,
};
use tokio::{
    fs,
    io::AsyncWriteExt,
    sync::{broadcast, Mutex},
};
use tracing::Instrument;
use uuid::Uuid;

const MAX_FILE: u64 = 1024 * 1024 * 1024;
const MAX_TEXT: usize = 128 * 1024;

type ApiResult<T> = Result<T, (StatusCode, String)>;

#[derive(Clone)]
struct AppState {
    key: String,
    url: String,
    dir: PathBuf,
    shared_text: Arc<Mutex<SharedText>>,
    text_path: PathBuf,
    changes: broadcast::Sender<&'static str>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct SharedText {
    text: String,
    revision: u64,
}
#[derive(Deserialize)]
struct TextInput {
    text: String,
}

// Log fixed operation names, never request URLs, headers, filenames or bodies.
async fn request_log(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let operation = operation_name(request.method().as_str(), request.uri().path());
    let request_id = Uuid::new_v4().to_string();
    let span = tracing::info_span!("request", %request_id, operation, ip = %peer.ip());
    async move {
        let started = Instant::now();
        let mut response = next.run(request).await;
        let status = response.status();
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if status.is_client_error() || status.is_server_error() {
            tracing::warn!(
                status = status.as_u16(),
                elapsed_ms,
                reason = status.canonical_reason().unwrap_or("unknown"),
                "request_failed"
            );
        } else if matches!(operation, "upload" | "download" | "share_text" | "events") {
            // In particular, this is NOT a download-completed event.
            tracing::info!(status = status.as_u16(), elapsed_ms, "response_ready");
        }
        response
            .headers_mut()
            .insert("x-request-id", request_id.parse().unwrap());
        response
    }
    .instrument(span)
    .await
}

fn operation_name(method: &str, path: &str) -> &'static str {
    match (method, path) {
        ("GET", "/") => "page",
        ("GET", "/api/files") => "list_files",
        ("POST", "/api/files") => "upload",
        ("GET", "/api/text") => "read_text",
        ("POST", "/api/text") => "share_text",
        ("GET", "/api/events") => "events",
        ("GET", path) if path.starts_with("/api/files/") => "download",
        _ => "unknown",
    }
}

async fn events(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    if query.key.as_deref() != Some(state.key.as_str()) {
        return Err((StatusCode::UNAUTHORIZED, "无效的访问密钥".into()));
    }
    // Browser connections must originate from this same service.
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok());
    if !matches!((host, origin), (Some(host), Some(origin)) if origin == format!("http://{host}")) {
        return Err((StatusCode::FORBIDDEN, "不允许的连接来源".into()));
    }
    let changes = state.changes.subscribe();
    let span = tracing::Span::current();
    Ok(ws
        .max_message_size(1024)
        .on_upgrade(move |socket| send_changes(socket, changes).instrument(span)))
}

async fn send_changes(mut socket: WebSocket, mut changes: broadcast::Receiver<&'static str>) {
    tracing::info!("events_connected");
    // Subscribe before announcing readiness so initial HTTP reads cannot miss a change.
    if socket.send(Message::Text("ready".into())).await.is_err() {
        tracing::warn!("events_send_failed");
        return;
    }
    loop {
        tokio::select! {
            change = changes.recv() => {
                let event = match change {
                    Ok(event) => event,
                    Err(broadcast::error::RecvError::Lagged(_)) => "refresh",
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                if socket.send(Message::Text(event.into())).await.is_err() {
                    tracing::warn!("events_send_failed");
                    break;
                }
            }
            message = socket.recv() => {
                match message {
                    None | Some(Ok(Message::Close(_))) => break,
                    Some(Err(_)) => {
                        tracing::warn!("events_receive_failed");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    tracing::info!("events_disconnected");
}
#[derive(Deserialize)]
struct PageQuery {
    key: Option<String>,
}
#[derive(Serialize)]
struct FileEntry {
    id: String,
    name: String,
    size: u64,
}

fn authorize(headers: &HeaderMap, state: &AppState) -> ApiResult<()> {
    if headers.get("x-access-key").and_then(|v| v.to_str().ok()) == Some(state.key.as_str()) {
        Ok(())
    } else {
        Err((StatusCode::UNAUTHORIZED, "无效的访问密钥".into()))
    }
}

async fn index(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> ApiResult<Html<String>> {
    if query.key.as_deref() != Some(state.key.as_str()) {
        return Err((
            StatusCode::UNAUTHORIZED,
            "请扫描电脑屏幕上的二维码，或使用终端中的完整链接。".into(),
        ));
    }
    let page = include_str!("../static/index.html").replace("__KEY__", &state.key);
    Ok(Html(page))
}

async fn list_files(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<FileEntry>>> {
    authorize(&headers, &state)?;
    let mut entries = fs::read_dir(&state.dir).await.map_err(internal)?;
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(internal)? {
        if !entry.file_type().await.map_err(internal)?.is_file() {
            continue;
        }
        let metadata = entry.metadata().await.map_err(internal)?;
        let id = entry.file_name().to_string_lossy().into_owned();
        files.push((
            metadata.modified().ok(),
            FileEntry {
                name: display_name(&id),
                size: metadata.len(),
                id,
            },
        ));
    }
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    Ok(Json(files.into_iter().map(|(_, file)| file).collect()))
}

async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<Json<Vec<FileEntry>>> {
    authorize(&headers, &state)?;
    let mut uploaded = Vec::new();
    while let Some(mut field) = multipart.next_field().await.map_err(bad_request)? {
        let declared = field
            .headers()
            .get(CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .and_then(disposition_file_name)
            .or_else(|| {
                field
                    .file_name()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            });
        let name = declared
            .as_deref()
            .map(safe_name)
            .unwrap_or_else(|| fallback_name(field.content_type()));
        let (mut file, id, path) = create_shared_file(&state.dir, &name)
            .await
            .map_err(internal)?;
        let mut size = 0u64;
        let result: ApiResult<()> = async {
            while let Some(chunk) = field.chunk().await.map_err(bad_request)? {
                size += chunk.len() as u64;
                if size > MAX_FILE {
                    return Err((StatusCode::PAYLOAD_TOO_LARGE, "单个文件最大 1 GiB".into()));
                }
                file.write_all(&chunk).await.map_err(internal)?;
            }
            file.flush().await.map_err(internal)?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(err) = result {
            let _ = fs::remove_file(&path).await;
            return Err(err);
        }
        if declared.is_none() && size == 0 {
            let _ = fs::remove_file(&path).await;
            continue;
        }
        uploaded.push(FileEntry {
            name: display_name(&id),
            id,
            size,
        });
        tracing::info!(bytes = size, "file_stored");
        let _ = state.changes.send("files_changed");
    }
    if uploaded.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "没有收到任何文件。请重新选择文件；如果使用微信等内置浏览器，请改用系统浏览器打开链接。"
                .into(),
        ));
    }
    Ok(Json(uploaded))
}

async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    authorize(&headers, &state)?;
    if id.is_empty()
        || id == "."
        || id == ".."
        || id.contains('/')
        || id.contains('\\')
        || id.contains('\0')
    {
        return Err((StatusCode::BAD_REQUEST, "无效文件名".into()));
    }
    let path = state.dir.join(&id);
    let Ok(metadata) = fs::metadata(&path).await else {
        return Err((StatusCode::NOT_FOUND, "文件不存在".into()));
    };
    if !metadata.is_file() {
        return Err((StatusCode::NOT_FOUND, "文件不存在".into()));
    }
    let file = fs::File::open(&path).await.map_err(internal)?;
    let name = display_name(&id);
    let encoded: String = name.bytes().map(|b| format!("%{b:02X}")).collect();
    let span = tracing::Span::current();
    let stream = tokio_util::io::ReaderStream::new(file).inspect_err(move |error| {
        tracing::warn!(parent: &span, kind = ?error.kind(), "download_stream_failed");
    });
    tracing::info!(bytes = metadata.len(), "download_started");
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename*=UTF-8''{encoded}"),
            ),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

async fn get_text(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<SharedText>> {
    authorize(&headers, &state)?;
    Ok(Json(state.shared_text.lock().await.clone()))
}

async fn set_text(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<TextInput>,
) -> ApiResult<Json<SharedText>> {
    authorize(&headers, &state)?;
    if input.text.len() > MAX_TEXT {
        return Err((StatusCode::PAYLOAD_TOO_LARGE, "文本最大 128 KiB".into()));
    }
    // Serialize writers across persistence and publication.
    let mut current = state.shared_text.lock().await;
    if current.text == input.text {
        return Ok(Json(current.clone()));
    }
    let next = SharedText {
        text: input.text,
        revision: current
            .revision
            .checked_add(1)
            .ok_or_else(|| (StatusCode::INTERNAL_SERVER_ERROR, "文本版本已达上限".into()))?,
    };
    persist_text(&state.text_path, &next)
        .await
        .map_err(internal)?;
    *current = next;
    let _ = state.changes.send("text_changed");
    tracing::info!(
        revision = current.revision,
        bytes = current.text.len(),
        "text_stored"
    );
    Ok(Json(current.clone()))
}

async fn load_text(path: &FilePath) -> io::Result<SharedText> {
    let bytes = match fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(SharedText::default()),
        Err(error) => return Err(error),
    };
    let text: SharedText = serde_json::from_slice(&bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "文本状态文件损坏"))?;
    if text.text.len() > MAX_TEXT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "保存的文本超过大小限制",
        ));
    }
    Ok(text)
}

async fn persist_text(path: &FilePath, text: &SharedText) -> io::Result<()> {
    let bytes = serde_json::to_vec(text).map_err(io::Error::other)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("缺少状态目录"))?;
    let temporary = parent.join(format!(".{}.tmp", Uuid::new_v4()));
    let result = async {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(&temporary, path).await
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("file");
    let clean: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .take(120)
        .collect();
    if clean.is_empty() || clean == "." || clean == ".." {
        "file".into()
    } else {
        clean
    }
}

/// Older uploads were stored as `uuid__original-name`. Strip that prefix for
/// display so both old and hand-copied files read the same in the page.
fn display_name(id: &str) -> String {
    match id.split_once("__") {
        Some((prefix, rest)) if !rest.is_empty() && Uuid::parse_str(prefix).is_ok() => rest.into(),
        _ => id.into(),
    }
}

/// Create `name` inside the shared folder, numbering duplicates instead of
/// overwriting a file someone may have dropped in there by hand.
async fn create_shared_file(dir: &FilePath, name: &str) -> io::Result<(fs::File, String, PathBuf)> {
    for attempt in 0..1000 {
        let candidate = numbered_name(name, attempt);
        let path = dir.join(&candidate);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
        {
            Ok(file) => return Ok((file, candidate, path)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "同名文件过多，请先清理共享目录",
    ))
}

fn numbered_name(name: &str, attempt: u32) -> String {
    if attempt == 0 {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            format!("{stem} ({attempt}).{extension}")
        }
        _ => format!("{name} ({attempt})"),
    }
}

/// Reads the file name out of a `Content-Disposition` header.
///
/// Browsers disagree here: most send `filename="..."`, but some Android and
/// in-app webviews only send the RFC 5987 form `filename*=UTF-8''...`, which
/// `multer` does not understand. Both are supported so those uploads are still
/// stored instead of being dropped.
fn disposition_file_name(disposition: &str) -> Option<String> {
    let mut plain = None;
    let mut encoded = None;
    for (key, value) in disposition_params(disposition) {
        if key.eq_ignore_ascii_case("filename*") {
            encoded = Some(decode_rfc5987(value));
        } else if key.eq_ignore_ascii_case("filename") && plain.is_none() {
            plain = Some(value.to_string());
        }
    }
    encoded.or(plain)
}

fn disposition_params(disposition: &str) -> Vec<(&str, &str)> {
    let mut params = Vec::new();
    let mut rest = match disposition.find(';') {
        Some(pos) => &disposition[pos + 1..],
        None => return params,
    };
    loop {
        let Some(assign) = rest.find('=') else {
            return params;
        };
        let key = rest[..assign].trim();
        let after = rest[assign + 1..].trim_start();
        if let Some(quoted) = after.strip_prefix('"') {
            match quoted.find('"') {
                Some(end) => {
                    params.push((key, &quoted[..end]));
                    rest = &quoted[end + 1..];
                }
                None => {
                    params.push((key, quoted.trim_end()));
                    return params;
                }
            }
        } else {
            let end = after.find(';').unwrap_or(after.len());
            params.push((key, after[..end].trim()));
            rest = &after[end..];
        }
        match rest.find(';') {
            Some(pos) => rest = &rest[pos + 1..],
            None => return params,
        }
    }
}

fn decode_rfc5987(value: &str) -> String {
    let raw = value.splitn(3, '\'').nth(2).unwrap_or(value);
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = hex {
                decoded.push(byte);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// Some phone browsers send a file part with no name at all. Derive something
/// readable from the content type instead of discarding the upload.
fn fallback_name(content_type: Option<&str>) -> String {
    let extension = content_type
        .and_then(|value| value.split(';').next())
        .and_then(|mime| mime.trim().split_once('/'))
        .map(|(_, subtype)| subtype.trim())
        .filter(|subtype| {
            !subtype.is_empty()
                && subtype.len() <= 5
                && subtype.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .map(|subtype| if subtype == "jpeg" { "jpg" } else { subtype });
    match extension {
        Some(extension) => format!("upload.{extension}"),
        None => "upload".into(),
    }
}
fn internal(e: io::Error) -> (StatusCode, String) {
    tracing::error!(kind = ?e.kind(), os_code = ?e.raw_os_error(), "storage_failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "存储操作失败，请查看服务端日志".into(),
    )
}
fn bad_request(_e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, "上传数据无效或传输中断".into())
}

fn local_ip() -> Option<String> {
    let output = Command::new("ip")
        .args(["-o", "-4", "addr", "show"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let preferred = std::env::var("LAN_INTERFACE").ok();
    select_lan_ip(
        &String::from_utf8_lossy(&output.stdout),
        preferred.as_deref(),
    )
}

fn select_lan_ip(output: &str, preferred: Option<&str>) -> Option<String> {
    let mut addresses = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let Some(pos) = fields.iter().position(|part| *part == "inet") else {
            continue;
        };
        if pos < 2 || pos + 1 >= fields.len() {
            continue;
        }
        let interface = fields[1]
            .trim_end_matches(':')
            .split('@')
            .next()
            .unwrap_or("");
        let Ok(ip) = fields[pos + 1]
            .split('/')
            .next()
            .unwrap_or("")
            .parse::<Ipv4Addr>()
        else {
            continue;
        };
        if ip.is_loopback() || ip.is_link_local() || !ip.is_private() {
            continue;
        }
        if let Some(name) = preferred {
            if interface == name {
                return Some(ip.to_string());
            }
            continue;
        }
        let priority = if interface.starts_with("wl") {
            3
        } else if interface.starts_with("en") || interface.starts_with("eth") {
            2
        } else {
            1
        };
        addresses.push((priority, ip));
    }
    addresses.sort_by_key(|(priority, _)| std::cmp::Reverse(*priority));
    addresses.first().map(|(_, ip)| ip.to_string())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let command = cli::parse(std::env::args_os().skip(1))
        .map_err(|message| format!("{message}\n\n{}", cli::HELP))?;
    if command == cli::Command::Help {
        println!("{}", cli::HELP);
        return Ok(());
    }
    let paths = cli::Paths::from_env()?;
    match command {
        cli::Command::Log => {
            cli::show_log(&paths.log)?;
            return Ok(());
        }
        cli::Command::Share(source) => {
            let destination = cli::share(&source, &paths.shared).await?;
            println!("已分享到：{}", destination.display());
            println!("已打开的页面请点击“刷新文件列表”。");
            return Ok(());
        }
        cli::Command::Serve => {}
        cli::Command::Help => unreachable!(),
    }
    let log = logging::CappedLog::open(&paths.log, logging::MAX_LOG_BYTES)?;
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .log_internal_errors(false)
        .with_writer(std::sync::Mutex::new(log))
        .init();
    if let Err(error) = run(paths.shared).await {
        // Startup errors are constructed without access keys or text bodies.
        tracing::error!(reason = %error, "startup_failed");
        return Err(format!("服务启动或运行失败，请运行 lan-bridge log 查看日志：{error}").into());
    }
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/files", get(list_files).post(upload))
        .route("/api/files/{id}", get(download))
        .route("/api/text", get(get_text).post(set_text))
        .route("/api/events", get(events))
        .layer(DefaultBodyLimit::max((MAX_FILE + 1024 * 1024) as usize))
        .layer(middleware::from_fn(request_log))
        .with_state(state)
}

async fn run(dir: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let ip = std::env::var("LAN_IP")
        .ok()
        .or_else(local_ip)
        .ok_or("未找到局域网 IPv4 地址。可设置 LAN_INTERFACE")?;
    let key = Uuid::new_v4().to_string();
    fs::create_dir_all(&dir).await?;
    let state_dir = dir.join(".lan-bridge-state");
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(&state_dir).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let text_path = state_dir.join("latest-text.json");
    let shared_text = load_text(&text_path).await?;
    let (changes, _) = broadcast::channel(64);
    let state = AppState {
        url: format!("http://{ip}:{port}/?key={key}"),
        key,
        dir,
        shared_text: Arc::new(Mutex::new(shared_text)),
        text_path,
        changes,
    };
    let app = app(state.clone());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    let terminal_qr = QrCode::new(state.url.as_bytes())?
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    let color = io::stdout().is_terminal();
    let (cyan, green, dim, reset) = if color {
        ("\x1b[96m", "\x1b[92m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "", "")
    };
    println!("\n{cyan}╭──────────────────────────────────────────────╮");
    println!("│  LAN / BRIDGE                         v0.1  │");
    println!("╰──────────────────────────────────────────────╯{reset}");
    println!("  {green}● ONLINE{reset}   {dim}手机与电脑连接同一局域网{reset}\n");
    println!("  {cyan}SCAN TO CONNECT{reset}\n{terminal_qr}");
    println!("  {cyan}LINK{reset}    {}", state.url);
    println!("  {cyan}FILES{reset}   {}", state.dir.display());
    println!("  {dim}按 Ctrl+C 关闭服务{reset}\n");
    tracing::info!(port, "server_started");
    tokio::spawn(browser::open_chrome(state.url.clone()));
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use tower::ServiceExt;

    pub(crate) struct TestState(pub(crate) AppState);

    impl TestState {
        pub(crate) async fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("lan-bridge-test-{}", Uuid::new_v4()));
            fs::create_dir_all(dir.join(".lan-bridge-state"))
                .await
                .unwrap();
            let (changes, _) = broadcast::channel(64);
            Self(AppState {
                key: "test-secret".into(),
                url: String::new(),
                text_path: dir.join(".lan-bridge-state/latest-text.json"),
                dir,
                shared_text: Arc::new(Mutex::new(SharedText::default())),
                changes,
            })
        }

        async fn request(
            &self,
            method: &str,
            path: &str,
            body: Body,
            authorized: bool,
        ) -> Response {
            self.request_with_type(method, path, body, authorized, "application/json")
                .await
        }

        async fn request_with_type(
            &self,
            method: &str,
            path: &str,
            body: Body,
            authorized: bool,
            content_type: &str,
        ) -> Response {
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, content_type);
            if authorized {
                request = request.header("x-access-key", &self.0.key);
            }
            let mut request = request.body(body).unwrap();
            request.extensions_mut().insert(ConnectInfo(
                "127.0.0.1:12345".parse::<SocketAddr>().unwrap(),
            ));
            app(self.0.clone()).oneshot(request).await.unwrap()
        }
    }

    impl Drop for TestState {
        fn drop(&mut self) {
            // Only this test's freshly created, uniquely named directory.
            let _ = std::fs::remove_dir_all(&self.0.dir);
        }
    }

    async fn response_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn persists_latest_text_and_publishes_only_changes() {
        let state = TestState::new().await;
        let mut notifications = state.0.changes.subscribe();
        for (text, expected_revision) in [("first", 1), ("first", 1), ("最新文本", 2), ("", 3)]
        {
            let response = state
                .request(
                    "POST",
                    "/api/text",
                    Body::from(serde_json::json!({"text": text}).to_string()),
                    true,
                )
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.headers().contains_key("x-request-id"));
            let saved = load_text(&state.0.text_path).await.unwrap();
            assert_eq!(saved.text, text);
            assert_eq!(saved.revision, expected_revision);
        }
        for _ in 0..3 {
            assert_eq!(notifications.try_recv().unwrap(), "text_changed");
        }
        assert!(notifications.try_recv().is_err());
        let files = state
            .request("GET", "/api/files", Body::empty(), true)
            .await;
        assert_eq!(response_text(files).await, "[]");
        let response = state
            .request("GET", "/api/files/.lan-bridge-state", Body::empty(), true)
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = state
            .request(
                "GET",
                "/api/files/.lan-bridge-state%2Flatest-text.json",
                Body::empty(),
                true,
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn failed_persistence_preserves_memory_and_does_not_notify() {
        let mut state = TestState::new().await;
        // A directory cannot be replaced by the state file.
        state.0.text_path = state.0.dir.join(".lan-bridge-state");
        let mut notifications = state.0.changes.subscribe();
        let response = state
            .request(
                "POST",
                "/api/text",
                Body::from(r#"{"text":"private"}"#),
                true,
            )
            .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(state.0.shared_text.lock().await.revision, 0);
        assert!(state.0.shared_text.lock().await.text.is_empty());
        assert!(notifications.try_recv().is_err());
        assert!(!response_text(response).await.contains("private"));
        let mut entries = fs::read_dir(&state.0.dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            assert!(!entry.file_name().to_string_lossy().ends_with(".tmp"));
        }
    }

    #[tokio::test]
    async fn rejects_unauthorized_oversized_and_malformed_text() {
        let state = TestState::new().await;
        for (body, authorized, status) in [
            (
                r#"{"text":"secret"}"#.to_string(),
                false,
                StatusCode::UNAUTHORIZED,
            ),
            (
                serde_json::json!({"text": "x".repeat(MAX_TEXT + 1)}).to_string(),
                true,
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
            ("{".into(), true, StatusCode::BAD_REQUEST),
        ] {
            let response = state
                .request("POST", "/api/text", Body::from(body), authorized)
                .await;
            assert_eq!(response.status(), status);
        }
        assert_eq!(state.0.shared_text.lock().await.revision, 0);
        assert!(!state.0.text_path.exists());
        let response = state
            .request("GET", "/api/text", Body::empty(), false)
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn concurrent_text_updates_leave_disk_and_memory_consistent() {
        let state = TestState::new().await;
        let (first, second) = tokio::join!(
            state.request("POST", "/api/text", Body::from(r#"{"text":"a"}"#), true),
            state.request("POST", "/api/text", Body::from(r#"{"text":"b"}"#), true),
        );
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(second.status(), StatusCode::OK);
        let saved = load_text(&state.0.text_path).await.unwrap();
        assert_eq!(saved.text, state.0.shared_text.lock().await.text);
        assert_eq!(saved.revision, 2);
    }

    #[tokio::test]
    async fn missing_state_is_empty_but_corrupt_state_is_not_silently_discarded() {
        let state = TestState::new().await;
        assert_eq!(load_text(&state.0.text_path).await.unwrap().revision, 0);
        fs::write(&state.0.text_path, b"{broken").await.unwrap();
        assert_eq!(
            load_text(&state.0.text_path).await.err().unwrap().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn uploaded_files_notify_and_remain_downloadable() {
        let state = TestState::new().await;
        let mut notifications = state.0.changes.subscribe();
        let response = state.request_with_type("POST", "/api/files", Body::from(
            "--test-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"private-name.txt\"\r\nContent-Type: text/plain\r\n\r\nfile content\r\n--test-boundary--\r\n"
        ), true, "multipart/form-data; boundary=test-boundary").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(notifications.try_recv().unwrap(), "files_changed");
        let response = state
            .request("GET", "/api/files/private-name.txt", Body::empty(), true)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_text(response).await, "file content");
        let response = state
            .request("GET", "/api/files", Body::empty(), true)
            .await;
        let body = response_text(response).await;
        assert!(body.contains("private-name.txt"));
        assert!(!body.contains("latest-text.json"));
    }

    #[tokio::test]
    async fn restart_restores_text_for_http_reads() {
        let mut state = TestState::new().await;
        persist_text(
            &state.0.text_path,
            &SharedText {
                text: "saved text".into(),
                revision: 7,
            },
        )
        .await
        .unwrap();
        state.0.shared_text = Arc::new(Mutex::new(load_text(&state.0.text_path).await.unwrap()));
        let response = state.request("GET", "/api/text", Body::empty(), true).await;
        let restored: SharedText = serde_json::from_str(&response_text(response).await).unwrap();
        assert_eq!(restored.text, "saved text");
        assert_eq!(restored.revision, 7);
    }

    #[tokio::test]
    async fn websocket_requires_key_and_same_origin_then_notifies_changes() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let state = TestState::new().await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_app = app(state.0.clone());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                server_app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        for (key, origin, expected) in [
            (
                "wrong",
                format!("http://{address}"),
                StatusCode::UNAUTHORIZED,
            ),
            (
                "test-secret",
                "http://other.example".into(),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let mut request = format!("ws://{address}/api/events?key={key}")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
            let error = tokio_tungstenite::connect_async(request)
                .await
                .err()
                .unwrap();
            match error {
                tokio_tungstenite::tungstenite::Error::Http(response) => {
                    assert_eq!(response.status(), expected)
                }
                error => panic!("unexpected websocket error: {error}"),
            }
        }
        let mut request = format!("ws://{address}/api/events?key=test-secret")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::ORIGIN, format!("http://{address}").parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "ready"
        );
        let response = state
            .request("POST", "/api/text", Body::from(r#"{"text":"hello"}"#), true)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let event = tokio::time::timeout(std::time::Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(event.into_text().unwrap(), "text_changed");
        state.0.changes.send("files_changed").unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            "files_changed"
        );
        socket.close(None).await.unwrap();
        server.abort();
    }

    #[test]
    fn log_operations_never_contain_user_paths() {
        assert_eq!(
            operation_name("GET", "/api/files/private-name.txt"),
            "download"
        );
        assert_eq!(operation_name("GET", "/private-text"), "unknown");
    }

    #[test]
    fn chooses_wifi_over_virtual_route() {
        let interfaces = "1: lo    inet 127.0.0.1/8 scope host lo\n\
                          2: tun0  inet 198.18.0.1/15 scope global tun0\n\
                          3: en-test inet 192.168.2.10/24 scope global en-test\n\
                          4: wl-test inet 192.168.1.95/24 scope global wl-test\n";
        assert_eq!(
            select_lan_ip(interfaces, None).as_deref(),
            Some("192.168.1.95")
        );
        assert_eq!(
            select_lan_ip(interfaces, Some("en-test")).as_deref(),
            Some("192.168.2.10")
        );
    }

    #[test]
    fn reads_quoted_and_unquoted_filenames() {
        assert_eq!(
            disposition_file_name(r#"form-data; name="file"; filename="photo.jpg""#).as_deref(),
            Some("photo.jpg")
        );
        assert_eq!(
            disposition_file_name(r#"form-data; name="file"; filename=photo.jpg"#).as_deref(),
            Some("photo.jpg")
        );
        assert_eq!(
            disposition_file_name(r#"form-data; name="file"; filename="带 空格.jpg""#).as_deref(),
            Some("带 空格.jpg")
        );
    }

    #[test]
    fn reads_rfc5987_filename_from_phone_webviews() {
        let disposition = "form-data; name=\"file\"; filename*=UTF-8''%E6%B5%8B%E8%AF%95.jpg";
        assert_eq!(
            disposition_file_name(disposition).as_deref(),
            Some("测试.jpg")
        );
    }

    #[test]
    fn prefers_rfc5987_filename_over_plain_one() {
        let disposition = "form-data; name=\"file\"; filename=\"fallback.jpg\"; \
                           filename*=UTF-8''%E7%9C%9F%E5%AE%9E.jpg";
        assert_eq!(
            disposition_file_name(disposition).as_deref(),
            Some("真实.jpg")
        );
    }

    #[test]
    fn ignores_parts_without_filename() {
        assert!(disposition_file_name(r#"form-data; name="file""#).is_none());
        assert!(disposition_file_name("form-data").is_none());
    }

    #[test]
    fn derives_extension_from_content_type() {
        assert_eq!(fallback_name(Some("image/jpeg")), "upload.jpg");
        assert_eq!(fallback_name(Some("video/mp4; codecs=avc1")), "upload.mp4");
        assert_eq!(fallback_name(Some("application/octet-stream")), "upload");
        assert_eq!(fallback_name(None), "upload");
    }

    #[test]
    fn strips_windows_paths_and_keeps_unicode() {
        assert_eq!(safe_name(r"D:\fakepath\照片 1.jpg"), "照片 1.jpg");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".."), "file");
    }

    #[test]
    fn numbers_duplicate_file_names() {
        assert_eq!(numbered_name("photo.jpg", 0), "photo.jpg");
        assert_eq!(numbered_name("photo.jpg", 2), "photo (2).jpg");
        assert_eq!(numbered_name("archive.tar.gz", 1), "archive.tar (1).gz");
        assert_eq!(numbered_name("README", 1), "README (1)");
        assert_eq!(numbered_name(".bashrc", 1), ".bashrc (1)");
    }

    #[test]
    fn hides_legacy_uuid_prefix() {
        assert_eq!(
            display_name("2b6d1431-1505-4cc2-8d1a-7f540b4d6e0d__照片.jpg"),
            "照片.jpg"
        );
        assert_eq!(display_name("照片.jpg"), "照片.jpg");
        assert_eq!(display_name("not-a-uuid__x.jpg"), "not-a-uuid__x.jpg");
    }
}
