use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{Emitter, Manager};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Task {
    id: i64,
    title: String,
    source: String,
    duration: String,
    state: String,
    progress: i64,
    created_at: String,
    input_ref: String,
    subtitle_source: Option<String>,
    error_message: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelConfig {
    model: String,
    summary_prompt: String,
    configured: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiCall {
    id: i64,
    created_at: String,
    purpose: String,
    provider: String,
    base_url: String,
    model: String,
    success: bool,
    status_code: Option<i64>,
    duration_ms: i64,
    error_message: Option<String>,
    trace_id: Option<String>,
    request_headers: Option<String>,
    request_body: Option<String>,
    response_headers: Option<String>,
    response_body: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct SummaryStreamDelta {
    content: String,
    stage: String,
    reset: bool,
}

#[derive(Debug, Serialize)]
struct RuntimeStatus {
    ffmpeg: bool,
    whisper: bool,
    model: bool,
    ytdlp: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlatformSubtitles {
    transcript: String,
    source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaMetadata {
    title: String,
    duration: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Preferences {
    cleanup_temp_media: bool,
    transcription_language: String,
    summary_timeout_manual: bool,
    summary_timeout_minutes: u64,
}

const BASE_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin?download=true";
const BASE_MODEL_SHA1: &str = "465707469ff3a37a2b9b8d8f89f2f99de7299dac";
const SILICONFLOW_BASE_URL: &str = "https://api.siliconflow.cn/v1";
const SILICONFLOW_PROVIDER: &str = "SiliconFlow";
const LEGACY_APP_DATA_DIR: &str = "com.maximusmol.video-knowledge";
const LEGACY_DATABASE_FILE: &str = "video-knowledge.sqlite3";
const DATABASE_FILE: &str = "quickvideo.sqlite3";
const DEFAULT_SUMMARY_PROMPT: &str = "请将下面的视频字幕整理为中文 Markdown 笔记。\n\n请包含：\n1. 一句话概览\n2. 3-5 条核心收获\n3. 按时间切分的章节：每章包含标题、时间范围和要点\n\n只基于字幕中的信息，不要编造事实。";
const API_KEY_TEST_TIMEOUT_SECONDS: u64 = 30;
const DEFAULT_SUMMARY_TIMEOUT_MINUTES: u64 = 10;
const SUMMARY_TIMEOUT_OPTIONS: [u64; 6] = [5, 10, 15, 20, 25, 30];
const TASK_CANCELLED: &str = "任务已由用户终止。";

#[derive(Clone, Default)]
struct TaskRegistry {
    controls: Arc<Mutex<HashMap<i64, Arc<AtomicBool>>>>,
}

impl TaskRegistry {
    fn control(&self, task_id: i64) -> Arc<AtomicBool> {
        let mut controls = self.controls.lock().expect("task registry lock poisoned");
        controls
            .entry(task_id)
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    fn cancel(&self, task_id: i64) -> bool {
        let controls = self.controls.lock().expect("task registry lock poisoned");
        let Some(control) = controls.get(&task_id) else {
            return false;
        };
        control.store(true, Ordering::SeqCst);
        true
    }

    fn release(&self, task_id: i64) {
        self.controls
            .lock()
            .expect("task registry lock poisoned")
            .remove(&task_id);
    }
}

fn run_task_command(
    registry: &TaskRegistry,
    task_id: i64,
    command: &mut Command,
) -> Result<Output, String> {
    let control = registry.control(task_id);
    if control.load(Ordering::SeqCst) {
        return Err(TASK_CANCELLED.into());
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动处理工具：{error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取处理工具输出。".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取处理工具错误输出。".to_string())?;
    let read_pipe = |mut pipe: std::process::ChildStdout| -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        Ok(bytes)
    };
    let stdout_reader = thread::spawn(move || read_pipe(stdout));
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut pipe = stderr;
        pipe.read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        Ok::<Vec<u8>, String>(bytes)
    });
    loop {
        if control.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(TASK_CANCELLED.into());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            let stdout = stdout_reader
                .join()
                .map_err(|_| "无法读取处理工具输出。".to_string())??;
            let stderr = stderr_reader
                .join()
                .map_err(|_| "无法读取处理工具错误输出。".to_string())??;
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn executable_name(tool: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{tool}.exe")
    } else {
        tool.to_string()
    }
}

fn bundled_tool_paths(tool: &str) -> Vec<PathBuf> {
    let executable = executable_name(tool);
    let mut paths = Vec::new();

    if let Some(directory) = env::var_os("QUICKVIDEO_TOOLS_DIR") {
        paths.push(PathBuf::from(directory).join(&executable));
    }

    if let Ok(current_exe) = env::current_exe() {
        if let Some(directory) = current_exe.parent() {
            // A portable Windows installation can keep its companion tools
            // beside the app in a `tools` directory.
            paths.push(directory.join("tools").join(&executable));

            #[cfg(target_os = "macos")]
            if let Some(contents) = directory.parent() {
                paths.push(contents.join("Resources").join("tools").join(&executable));
            }
        }
    }

    paths
}

fn tool_install_hint(tool: &str) -> String {
    if cfg!(target_os = "windows") {
        format!(
            "未找到 {tool}。请安装对应的 Windows 工具并将其加入 PATH，或把 {} 放到 QuickVideo.exe 同级的 tools 文件夹。",
            executable_name(tool)
        )
    } else {
        format!("未找到 {tool}。请安装对应工具并将其加入 PATH；macOS 可通过 Homebrew 安装。")
    }
}

/// Resolve tools from an explicitly configured directory, a bundled `tools`
/// directory, the process PATH, and the standard Homebrew locations on macOS.
fn tool_path(tool: &str) -> Result<PathBuf, String> {
    let executable = executable_name(tool);
    let mut candidates = bundled_tool_paths(tool);

    if let Some(path) = env::var_os("PATH") {
        candidates.extend(env::split_paths(&path).map(|directory| directory.join(&executable)));
    }

    #[cfg(target_os = "macos")]
    candidates.extend(
        ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
            .iter()
            .map(|directory| Path::new(directory).join(&executable)),
    );

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| tool_install_hint(tool))
}

fn ytdlp_command() -> Result<Command, String> {
    let ytdlp = tool_path("yt-dlp")?;
    let ffmpeg = tool_path("ffmpeg")?;
    let mut command = Command::new(ytdlp);

    // yt-dlp launches ffmpeg and ffprobe as child processes. Supplying their
    // directory keeps this reliable when the desktop app does not inherit the
    // interactive shell PATH (notably Finder on macOS).
    command.arg("--ffmpeg-location").arg(
        ffmpeg
            .parent()
            .ok_or_else(|| "无法确定 FFmpeg 安装目录。".to_string())?,
    );

    // YouTube extraction increasingly requires a JavaScript runtime. Deno is
    // supported by yt-dlp and uses the same cross-platform resolution logic.
    if let Ok(deno) = tool_path("deno") {
        command
            .arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    }
    Ok(command)
}

fn migrate_legacy_app_data(app: &tauri::AppHandle) -> Result<(), String> {
    let current_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&current_dir).map_err(|e| e.to_string())?;
    let Some(app_support_dir) = current_dir.parent() else {
        return Ok(());
    };
    let legacy_dir = app_support_dir.join(LEGACY_APP_DATA_DIR);

    let current_database = current_dir.join(DATABASE_FILE);
    let legacy_database = legacy_dir.join(LEGACY_DATABASE_FILE);
    if !current_database.exists() && legacy_database.is_file() {
        fs::copy(&legacy_database, &current_database)
            .map_err(|e| format!("无法迁移旧版资料库：{e}"))?;
    }

    let current_model = current_dir.join("models/ggml-base.bin");
    let legacy_model = legacy_dir.join("models/ggml-base.bin");
    if !current_model.exists() && legacy_model.is_file() {
        if let Some(parent) = current_model.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::hard_link(&legacy_model, &current_model)
            .or_else(|_| fs::copy(&legacy_model, &current_model).map(|_| ()))
            .map_err(|e| format!("无法迁移本地转写模型：{e}"))?;
    }
    Ok(())
}

fn base_model_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    migrate_legacy_app_data(app)?;
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("models/ggml-base.bin"))
}

fn database_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    migrate_legacy_app_data(app)?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(DATABASE_FILE))
}

#[tauri::command]
fn check_runtime(app: tauri::AppHandle) -> Result<RuntimeStatus, String> {
    let available = |tool: &str| {
        tool_path(tool).is_ok_and(|path| Command::new(path).arg("--version").output().is_ok())
    };
    let model = base_model_path(&app)?.exists();
    Ok(RuntimeStatus {
        ffmpeg: available("ffmpeg"),
        whisper: available("whisper-cli"),
        model,
        ytdlp: available("yt-dlp"),
    })
}

fn connection(app: &tauri::AppHandle) -> Result<Connection, String> {
    let conn = Connection::open(database_path(app)?).map_err(|e| e.to_string())?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS tasks (id INTEGER PRIMARY KEY, title TEXT NOT NULL, source TEXT NOT NULL, duration TEXT NOT NULL, state TEXT NOT NULL, progress INTEGER NOT NULL, created_at TEXT NOT NULL, input_ref TEXT NOT NULL DEFAULT '', subtitle_source TEXT, error_message TEXT); CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS notes (task_id INTEGER PRIMARY KEY, transcript TEXT NOT NULL, summary TEXT NOT NULL, updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS api_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, created_at TEXT NOT NULL, purpose TEXT NOT NULL, provider TEXT NOT NULL, base_url TEXT NOT NULL, model TEXT NOT NULL, success INTEGER NOT NULL, status_code INTEGER, duration_ms INTEGER NOT NULL, error_message TEXT);")
        .map_err(|e| e.to_string())?;
    for (column, definition) in [
        ("input_ref", "TEXT NOT NULL DEFAULT ''"),
        ("subtitle_source", "TEXT"),
        ("error_message", "TEXT"),
    ] {
        let present = conn
            .prepare("PRAGMA table_info(tasks)")
            .map_err(|e| e.to_string())?
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|name| name == column);
        if !present {
            conn.execute(
                &format!("ALTER TABLE tasks ADD COLUMN {column} {definition}"),
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    let call_columns = conn
        .prepare("PRAGMA table_info(api_calls)")
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    for (column, definition) in [
        ("trace_id", "TEXT"),
        ("request_headers", "TEXT"),
        ("request_body", "TEXT"),
        ("response_headers", "TEXT"),
        ("response_body", "TEXT"),
    ] {
        if !call_columns.iter().any(|name| name == column) {
            conn.execute(
                &format!("ALTER TABLE api_calls ADD COLUMN {column} {definition}"),
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(conn)
}

#[tauri::command]
fn save_note(
    app: tauri::AppHandle,
    task_id: i64,
    transcript: String,
    summary: String,
) -> Result<(), String> {
    let conn = connection(&app)?;
    conn.execute("INSERT INTO notes(task_id,transcript,summary,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id) DO UPDATE SET transcript=excluded.transcript,summary=excluded.summary,updated_at=excluded.updated_at", params![task_id, transcript, summary, chrono::Utc::now().to_rfc3339()]).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_note(app: tauri::AppHandle, task_id: i64) -> Result<(String, String), String> {
    let conn = connection(&app)?;
    conn.query_row(
        "SELECT transcript,summary FROM notes WHERE task_id=?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(|e| e.to_string())
}

fn setting(conn: &Connection, key: &str) -> Result<String, String> {
    conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |row| {
        row.get(0)
    })
    .map_err(|e| e.to_string())
}

fn configured_api_key(app: &tauri::AppHandle) -> Result<String, String> {
    let key = setting(&connection(app)?, "api_key")
        .map_err(|_| "请先在模型设置中保存 API Key。".to_string())?;
    if key.trim().is_empty() {
        return Err("请先在模型设置中保存 API Key。".into());
    }
    Ok(key)
}

#[tauri::command]
fn get_preferences(app: tauri::AppHandle) -> Result<Preferences, String> {
    let conn = connection(&app)?;
    Ok(Preferences {
        cleanup_temp_media: setting(&conn, "cleanup_temp_media")
            .map(|value| value == "true")
            .unwrap_or(true),
        transcription_language: setting(&conn, "transcription_language")
            .unwrap_or_else(|_| "自动识别（中文 / English）".into()),
        summary_timeout_manual: setting(&conn, "summary_timeout_manual")
            .map(|value| value == "true")
            .unwrap_or(false),
        summary_timeout_minutes: setting(&conn, "summary_timeout_minutes")
            .ok()
            .and_then(|value| value.parse().ok())
            .filter(|value| SUMMARY_TIMEOUT_OPTIONS.contains(value))
            .unwrap_or(DEFAULT_SUMMARY_TIMEOUT_MINUTES),
    })
}

#[tauri::command]
fn save_preferences(
    app: tauri::AppHandle,
    cleanup_temp_media: bool,
    transcription_language: String,
    summary_timeout_manual: bool,
    summary_timeout_minutes: u64,
) -> Result<(), String> {
    if !SUMMARY_TIMEOUT_OPTIONS.contains(&summary_timeout_minutes) {
        return Err("摘要最大等待时长必须为 5、10、15、20、25 或 30 分钟。".into());
    }
    let conn = connection(&app)?;
    for (key, value) in [
        ("cleanup_temp_media", cleanup_temp_media.to_string()),
        ("transcription_language", transcription_language),
        ("summary_timeout_manual", summary_timeout_manual.to_string()),
        (
            "summary_timeout_minutes",
            summary_timeout_minutes.to_string(),
        ),
    ] {
        conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn summary_timeout(app: &tauri::AppHandle) -> Duration {
    let preferences = get_preferences(app.clone()).unwrap_or(Preferences {
        cleanup_temp_media: true,
        transcription_language: "自动识别（中文 / English）".into(),
        summary_timeout_manual: false,
        summary_timeout_minutes: DEFAULT_SUMMARY_TIMEOUT_MINUTES,
    });
    let minutes = if preferences.summary_timeout_manual {
        preferences.summary_timeout_minutes
    } else {
        DEFAULT_SUMMARY_TIMEOUT_MINUTES
    };
    Duration::from_secs(minutes * 60)
}

fn cleanup_temp_media(app: &tauri::AppHandle) -> bool {
    connection(app)
        .ok()
        .and_then(|conn| setting(&conn, "cleanup_temp_media").ok())
        .map(|value| value == "true")
        .unwrap_or(true)
}

fn whisper_language(app: &tauri::AppHandle) -> String {
    match connection(app)
        .ok()
        .and_then(|conn| setting(&conn, "transcription_language").ok())
        .as_deref()
    {
        Some("中文") => "zh".into(),
        Some("English") => "en".into(),
        _ => "auto".into(),
    }
}

fn format_duration(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

#[tauri::command]
async fn get_local_duration(input_path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || get_local_duration_blocking(input_path))
        .await
        .map_err(|e| format!("后台媒体任务中断：{e}"))?
}

fn get_local_duration_blocking(input_path: String) -> Result<String, String> {
    let output = Command::new(tool_path("ffprobe")?)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            &input_path,
        ])
        .output()
        .map_err(|e| format!("无法启动 FFprobe：{e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let seconds = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .map_err(|_| "无法识别媒体时长。".to_string())?;
    Ok(format_duration(seconds))
}

#[tauri::command]
async fn get_link_metadata(url: String) -> Result<MediaMetadata, String> {
    tauri::async_runtime::spawn_blocking(move || get_link_metadata_blocking(url))
        .await
        .map_err(|e| format!("后台媒体任务中断：{e}"))?
}

fn get_link_metadata_blocking(url: String) -> Result<MediaMetadata, String> {
    if !(url.contains("youtube.com") || url.contains("youtu.be") || url.contains("bilibili.com")) {
        return Err("目前仅支持 YouTube 与 Bilibili 链接。".into());
    }
    let output = ytdlp_command()?
        .args([
            "--no-playlist",
            "--skip-download",
            "--print",
            "%(title)s",
            "--print",
            "%(duration_string)s",
            &url,
        ])
        .output()
        .map_err(|e| format!("无法读取视频信息：{e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let title = lines
        .first()
        .copied()
        .filter(|value| *value != "NA")
        .unwrap_or("未命名视频")
        .to_string();
    let duration = lines
        .get(1)
        .copied()
        .filter(|value| *value != "NA")
        .unwrap_or("待识别")
        .to_string();
    Ok(MediaMetadata { title, duration })
}

#[tauri::command]
fn download_base_model(app: tauri::AppHandle) -> Result<String, String> {
    let model = base_model_path(&app)?;
    if model.exists() && fs::metadata(&model).map_err(|e| e.to_string())?.len() > 100_000_000 {
        return Ok(model.to_string_lossy().into_owned());
    }
    let models_dir = model
        .parent()
        .ok_or_else(|| "无法创建模型目录。".to_string())?;
    fs::create_dir_all(models_dir).map_err(|e| e.to_string())?;
    let temporary = models_dir.join("ggml-base.bin.part");
    let _ = fs::remove_file(&temporary);
    let mut response = reqwest::blocking::Client::new()
        .get(BASE_MODEL_URL)
        .send()
        .map_err(|e| format!("下载模型失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("下载模型失败：{e}"))?;
    let mut file = fs::File::create(&temporary).map_err(|e| e.to_string())?;
    std::io::copy(&mut response, &mut file).map_err(|e| format!("写入模型失败：{e}"))?;
    file.flush().map_err(|e| e.to_string())?;
    let size = fs::metadata(&temporary).map_err(|e| e.to_string())?.len();
    if size < 100_000_000 {
        let _ = fs::remove_file(&temporary);
        return Err("下载的模型文件不完整，请检查网络后重试。".into());
    }
    if sha1_file(&temporary)? != BASE_MODEL_SHA1 {
        let _ = fs::remove_file(&temporary);
        return Err("模型校验失败，文件未被保存。请检查网络后重试。".into());
    }
    fs::rename(&temporary, &model).map_err(|e| e.to_string())?;
    Ok(model.to_string_lossy().into_owned())
}

fn sha1_file(path: &Path) -> Result<String, String> {
    #[cfg(target_os = "windows")]
    let output = Command::new("certutil")
        .args(["-hashfile"])
        .arg(path)
        .arg("SHA1")
        .output()
        .map_err(|e| format!("无法校验模型文件：{e}"))?;

    #[cfg(not(target_os = "windows"))]
    let output = Command::new("shasum")
        .args(["-a", "1"])
        .arg(path)
        .output()
        .map_err(|e| format!("无法校验模型文件：{e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }

    let checksum_output = String::from_utf8_lossy(&output.stdout);
    checksum_output
        .split_whitespace()
        .find(|value| {
            value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
        .map(str::to_ascii_lowercase)
        // `certutil` prints the Windows hash in groups separated by spaces.
        .or_else(|| {
            checksum_output
                .lines()
                .map(|line| {
                    line.chars()
                        .filter(|character| character.is_ascii_hexdigit())
                        .collect::<String>()
                })
                .find(|value| value.len() == 40)
                .map(|value| value.to_ascii_lowercase())
        })
        .ok_or_else(|| "无法读取模型文件的 SHA-1 校验值。".to_string())
}

#[tauri::command(rename_all = "camelCase")]
fn save_model_config(
    app: tauri::AppHandle,
    model: String,
    summary_prompt: String,
    api_key: String,
) -> Result<ModelConfig, String> {
    if !api_key.trim().is_empty() {
        let conn = connection(&app)?;
        conn.execute("INSERT INTO settings(key,value) VALUES('api_key',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![api_key]).map_err(|e| e.to_string())?;
    }
    let conn = connection(&app)?;
    let prompt = if summary_prompt.trim().is_empty() {
        DEFAULT_SUMMARY_PROMPT.to_string()
    } else {
        summary_prompt
    };
    for (key, value) in [("model", model), ("summary_prompt", prompt)] {
        conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value]).map_err(|e| e.to_string())?;
    }
    let saved = get_model_config(app)?;
    if !saved.configured {
        return Err("API Key 尚未写入本机配置。请重新输入后保存。".into());
    }
    Ok(saved)
}

#[tauri::command]
fn get_model_config(app: tauri::AppHandle) -> Result<ModelConfig, String> {
    let conn = connection(&app)?;
    let model = setting(&conn, "model").unwrap_or_else(|_| "Pro/zai-org/GLM-4.7".into());
    let summary_prompt =
        setting(&conn, "summary_prompt").unwrap_or_else(|_| DEFAULT_SUMMARY_PROMPT.into());
    let configured = setting(&conn, "api_key")
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false);
    Ok(ModelConfig {
        model,
        summary_prompt,
        configured,
    })
}

fn record_api_call(
    app: &tauri::AppHandle,
    config: &ModelConfig,
    purpose: &str,
    success: bool,
    status_code: Option<i64>,
    duration_ms: i64,
    error_message: Option<String>,
    trace_id: Option<String>,
    request_headers: String,
    request_body: String,
    response_headers: Option<String>,
    response_body: Option<String>,
) {
    let _ = connection(app).and_then(|conn| conn.execute("INSERT INTO api_calls(created_at,purpose,provider,base_url,model,success,status_code,duration_ms,error_message,trace_id,request_headers,request_body,response_headers,response_body) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)", params![chrono::Utc::now().to_rfc3339(), purpose, SILICONFLOW_PROVIDER, SILICONFLOW_BASE_URL, config.model, success, status_code, duration_ms, error_message, trace_id, request_headers, request_body, response_headers, response_body]).map_err(|e| e.to_string()));
}

fn chat_completion(
    app: &tauri::AppHandle,
    config: &ModelConfig,
    key: &str,
    purpose: &str,
    prompt: String,
    timeout: Duration,
) -> Result<String, String> {
    let started = Instant::now();
    let request_body = serde_json::to_string_pretty(&serde_json::json!({"model": config.model, "messages": [{"role":"user","content":prompt}], "temperature":0.2, "enable_thinking": false, "stream": false})).unwrap_or_default();
    let request_headers = format!(
        "Content-Type: application/json\nAuthorization: Bearer [REDACTED]\nUser-Agent: QuickVideo"
    );
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("无法创建直连网络客户端：{e}"))?;
    let response = client
        .post(format!("{SILICONFLOW_BASE_URL}/chat/completions"))
        .bearer_auth(key)
        .header("User-Agent", "QuickVideo")
        .body(request_body.clone())
        .send();
    let mut trace_id = None;
    let mut response_headers = None;
    let mut response_body = None;
    let result = match response {
        Ok(response) => {
            trace_id = response
                .headers()
                .get("x-siliconcloud-trace-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            response_headers = Some(
                response
                    .headers()
                    .iter()
                    .filter_map(|(name, value)| {
                        value.to_str().ok().map(|value| format!("{name}: {value}"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            let status = response.status().as_u16() as i64;
            let success = response.status().is_success();
            let body = response.text().unwrap_or_default();
            response_body = Some(body.clone());
            if !success {
                Err(format!(
                    "HTTP {status}：{}",
                    if body.trim().is_empty() {
                        "模型服务未返回错误详情"
                    } else {
                        body.trim()
                    }
                ))
            } else {
                serde_json::from_str::<serde_json::Value>(&body)
                    .map_err(|e| format!("无法解析模型服务响应：{e}"))
                    .and_then(|body| {
                        body["choices"][0]["message"]["content"]
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| "模型服务未返回可用摘要。".into())
                    })
            }
        }
        Err(error) => Err(format!("请求模型服务失败：{error}")),
    };
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let status_code = result
        .as_ref()
        .err()
        .and_then(|message| message.strip_prefix("HTTP "))
        .and_then(|message| message.split('：').next())
        .and_then(|code| code.parse::<i64>().ok());
    record_api_call(
        app,
        config,
        purpose,
        result.is_ok(),
        status_code,
        duration_ms,
        result.as_ref().err().cloned(),
        trace_id,
        request_headers,
        request_body,
        response_headers,
        response_body,
    );
    result
}

fn chat_completion_stream(
    app: &tauri::AppHandle,
    config: &ModelConfig,
    key: &str,
    purpose: &str,
    stage: &str,
    reset_preview: bool,
    prompt: String,
    deadline: Instant,
) -> Result<String, String> {
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| "摘要生成超过最大等待时长。".to_string())?;
    let started = Instant::now();
    let request_body = serde_json::to_string_pretty(&serde_json::json!({"model": config.model, "messages": [{"role":"user","content":prompt}], "temperature":0.2, "enable_thinking": false, "stream": true})).unwrap_or_default();
    let request_headers = "Accept: text/event-stream\nContent-Type: application/json\nAuthorization: Bearer [REDACTED]\nUser-Agent: QuickVideo".to_string();
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("无法创建直连网络客户端：{e}"))?;
    let response = client
        .post(format!("{SILICONFLOW_BASE_URL}/chat/completions"))
        .bearer_auth(key)
        .header("Accept", "text/event-stream")
        .header("User-Agent", "QuickVideo")
        .body(request_body.clone())
        .send();
    let mut trace_id = None;
    let mut response_headers = None;
    let mut response_body = None;
    let _ = app.emit(
        "summary-stream",
        SummaryStreamDelta {
            content: String::new(),
            stage: stage.to_string(),
            reset: reset_preview,
        },
    );
    let result = match response {
        Ok(response) => {
            trace_id = response
                .headers()
                .get("x-siliconcloud-trace-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            response_headers = Some(
                response
                    .headers()
                    .iter()
                    .filter_map(|(name, value)| {
                        value.to_str().ok().map(|value| format!("{name}: {value}"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            let status = response.status().as_u16() as i64;
            if !response.status().is_success() {
                let body = response.text().unwrap_or_default();
                response_body = Some(body.clone());
                Err(format!(
                    "HTTP {status}：{}",
                    if body.trim().is_empty() {
                        "模型服务未返回错误详情"
                    } else {
                        body.trim()
                    }
                ))
            } else {
                let mut text = String::new();
                let mut raw = String::new();
                let mut stream_error = None;
                for line in BufReader::new(response).lines() {
                    match line {
                        Ok(line) => {
                            raw.push_str(&line);
                            raw.push('\n');
                            let Some(data) = line.strip_prefix("data: ") else {
                                continue;
                            };
                            if data == "[DONE]" {
                                break;
                            }
                            match serde_json::from_str::<serde_json::Value>(data) {
                                Ok(chunk) => {
                                    if let Some(delta) =
                                        chunk["choices"][0]["delta"]["content"].as_str()
                                    {
                                        text.push_str(delta);
                                        let _ = app.emit(
                                            "summary-stream",
                                            SummaryStreamDelta {
                                                content: delta.to_string(),
                                                stage: stage.to_string(),
                                                reset: false,
                                            },
                                        );
                                    }
                                }
                                Err(error) => {
                                    stream_error = Some(format!("无法解析流式分片：{error}"));
                                    break;
                                }
                            }
                        }
                        Err(error) => {
                            stream_error = Some(format!("读取流式响应失败：{error}"));
                            break;
                        }
                    }
                }
                response_body = Some(raw);
                if let Some(error) = stream_error {
                    Err(error)
                } else if text.trim().is_empty() {
                    Err("模型服务未返回可用摘要。".into())
                } else {
                    Ok(text)
                }
            }
        }
        Err(error) => Err(format!("请求模型服务失败：{error}")),
    };
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let status_code = result
        .as_ref()
        .err()
        .and_then(|message| message.strip_prefix("HTTP "))
        .and_then(|message| message.split('：').next())
        .and_then(|code| code.parse::<i64>().ok());
    record_api_call(
        app,
        config,
        purpose,
        result.is_ok(),
        status_code,
        duration_ms,
        result.as_ref().err().cloned(),
        trace_id,
        request_headers,
        request_body,
        response_headers,
        response_body,
    );
    result
}

fn transcript_chunks(transcript: &str, max_bytes: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in transcript.lines() {
        if !current.is_empty() && current.len() + line.len() + 1 > max_bytes {
            chunks.push(current);
            current = String::new();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        chunks.push(current);
    }
    chunks
}

#[tauri::command]
async fn generate_summary(app: tauri::AppHandle, transcript: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || generate_summary_blocking(app, transcript))
        .await
        .map_err(|e| format!("后台摘要任务中断：{e}"))?
}

fn generate_summary_blocking(app: tauri::AppHandle, transcript: String) -> Result<String, String> {
    let config = get_model_config(app.clone())?;
    if !config.configured {
        return Err("请先在模型设置中保存 API Key。".into());
    }
    let key = configured_api_key(&app)?;
    let chunks = transcript_chunks(&transcript, 18_000);
    if chunks.is_empty() {
        return Err("字幕为空，无法生成摘要。".into());
    }
    let deadline = Instant::now() + summary_timeout(&app);
    if chunks.len() == 1 {
        return chat_completion_stream(
            &app,
            &config,
            &key,
            "流式生成摘要",
            "正在生成最终摘要",
            false,
            format!("{}\n\n字幕：\n{}", config.summary_prompt, chunks[0]),
            deadline,
        );
    }
    let total = chunks.len();
    let partials = chunks.iter().enumerate().map(|(index, chunk)| chat_completion_stream(&app, &config, &key, &format!("流式长视频分段 {}/{}", index + 1, total), &format!("正在提炼第 {} / {} 段", index + 1, total), false, format!("这是长视频字幕的第 {} / {} 段。请只基于本段内容提炼带时间范围的章节候选、事实与关键点，保留具体时间戳；不要给出全片结论，也不要编造。\n\n字幕：\n{chunk}", index + 1, total), deadline)).collect::<Result<Vec<_>, _>>()?;
    chat_completion_stream(&app, &config, &key, "流式合并长视频摘要", "正在合并最终摘要", true, format!("{}\n\n以下是按顺序提炼的长视频分段笔记。请只使用这些分段事实，删除重复，不要编造。\n\n{}", config.summary_prompt, partials.join("\n\n---\n\n")), deadline)
}

#[tauri::command]
async fn test_model_connection(app: tauri::AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || test_model_connection_blocking(app))
        .await
        .map_err(|e| format!("后台连接测试中断：{e}"))?
}

fn test_model_connection_blocking(app: tauri::AppHandle) -> Result<(), String> {
    let config = get_model_config(app.clone())?;
    if !config.configured {
        return Err("请先保存 API Key。".into());
    }
    let key = configured_api_key(&app)?;
    chat_completion(
        &app,
        &config,
        &key,
        "连接测试",
        "请只回复：连接成功".into(),
        Duration::from_secs(API_KEY_TEST_TIMEOUT_SECONDS),
    )
    .map(|_| ())
}

#[tauri::command]
fn list_api_calls(app: tauri::AppHandle) -> Result<Vec<ApiCall>, String> {
    let conn = connection(&app)?;
    let mut statement = conn.prepare("SELECT id,created_at,purpose,provider,base_url,model,success,status_code,duration_ms,error_message,trace_id,request_headers,request_body,response_headers,response_body FROM api_calls ORDER BY id DESC LIMIT 30").map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok(ApiCall {
                id: row.get(0)?,
                created_at: row.get(1)?,
                purpose: row.get(2)?,
                provider: row.get(3)?,
                base_url: row.get(4)?,
                model: row.get(5)?,
                success: row.get::<_, i64>(6)? != 0,
                status_code: row.get(7)?,
                duration_ms: row.get(8)?,
                error_message: row.get(9)?,
                trace_id: row.get(10)?,
                request_headers: row.get(11)?,
                request_body: row.get(12)?,
                response_headers: row.get(13)?,
                response_body: row.get(14)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let calls = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(calls)
}

#[tauri::command]
fn list_tasks(app: tauri::AppHandle) -> Result<Vec<Task>, String> {
    let conn = connection(&app)?;
    let mut stmt = conn.prepare("SELECT id,title,source,duration,state,progress,created_at,input_ref,subtitle_source,error_message FROM tasks ORDER BY id DESC").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Task {
                id: row.get(0)?,
                title: row.get(1)?,
                source: row.get(2)?,
                duration: row.get(3)?,
                state: row.get(4)?,
                progress: row.get(5)?,
                created_at: row.get(6)?,
                input_ref: row.get(7)?,
                subtitle_source: row.get(8)?,
                error_message: row.get(9)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn recover_incomplete_tasks(app: tauri::AppHandle) -> Result<usize, String> {
    let conn = connection(&app)?;
    conn.execute(
        "UPDATE tasks SET state='interrupted', progress=0, error_message='应用关闭后任务未完成，请重新开始。', created_at=?1 WHERE state IN ('queued', 'preparing', 'transcribing', 'summarizing', 'cancelling', 'orphaned')",
        [chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn cancel_task(task_id: i64, registry: tauri::State<'_, TaskRegistry>) -> Result<bool, String> {
    Ok(registry.cancel(task_id))
}

#[tauri::command]
fn activate_task(task_id: i64, registry: tauri::State<'_, TaskRegistry>) {
    registry.control(task_id).store(false, Ordering::SeqCst);
}

#[tauri::command]
fn release_task(task_id: i64, registry: tauri::State<'_, TaskRegistry>) {
    registry.release(task_id);
}

#[tauri::command]
fn create_task(
    app: tauri::AppHandle,
    title: String,
    source: String,
    input_ref: String,
) -> Result<Task, String> {
    let task = Task {
        id: chrono::Utc::now().timestamp_millis(),
        title,
        source,
        duration: "待识别".into(),
        state: "queued".into(),
        progress: 0,
        created_at: "已加入队列".into(),
        input_ref,
        subtitle_source: None,
        error_message: None,
    };
    let conn = connection(&app)?;
    conn.execute("INSERT INTO tasks(id,title,source,duration,state,progress,created_at,input_ref,subtitle_source,error_message) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![task.id, task.title, task.source, task.duration, task.state, task.progress, task.created_at, task.input_ref, task.subtitle_source, task.error_message]).map_err(|e| e.to_string())?;
    Ok(task)
}

#[tauri::command]
fn update_task(
    app: tauri::AppHandle,
    task_id: i64,
    state: String,
    progress: i64,
    title: Option<String>,
    duration: Option<String>,
    subtitle_source: Option<String>,
    error_message: Option<String>,
) -> Result<(), String> {
    let conn = connection(&app)?;
    conn.execute("UPDATE tasks SET state=?2, progress=?3, title=COALESCE(?4,title), duration=COALESCE(?5,duration), subtitle_source=COALESCE(?6,subtitle_source), error_message=?7, created_at=?8 WHERE id=?1", params![task_id, state, progress, title, duration, subtitle_source, error_message, chrono::Utc::now().to_rfc3339()]).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn update_task_metadata(
    app: tauri::AppHandle,
    task_id: i64,
    title: Option<String>,
    duration: Option<String>,
) -> Result<(), String> {
    let conn = connection(&app)?;
    conn.execute(
        "UPDATE tasks SET title=COALESCE(?2,title), duration=COALESCE(?3,duration) WHERE id=?1",
        params![task_id, title, duration],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn clear_tasks(app: tauri::AppHandle) -> Result<(), String> {
    let mut conn = connection(&app)?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM notes", [])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM tasks", [])
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

fn export_origin(source: &str, input_ref: &str) -> String {
    if source.contains("链接") {
        format!("> 原始链接：<{input_ref}>  \n")
    } else {
        let filename = Path::new(input_ref)
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .unwrap_or(input_ref);
        format!("> 本地文件：{filename}  \n")
    }
}

#[tauri::command]
fn export_markdown(
    app: tauri::AppHandle,
    title: String,
    task_id: Option<i64>,
    output_path: Option<String>,
    export_type: String,
) -> Result<String, String> {
    if !matches!(
        export_type.as_str(),
        "summaryAndTranscript" | "summary" | "transcript"
    ) {
        return Err("未知的导出类型。".into());
    }
    let path = match output_path {
        Some(value) => PathBuf::from(value),
        None => {
            let dir = app
                .path()
                .document_dir()
                .map_err(|e| e.to_string())?
                .join("QuickVideo");
            fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let filename = format!(
                "{}.md",
                title
                    .chars()
                    .map(|c| if "\\/:*?\"<>|".contains(c) { '-' } else { c })
                    .collect::<String>()
            );
            dir.join(filename)
        }
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = task_id
        .and_then(|id| {
            let conn = connection(&app).ok()?;
            let metadata = conn
                .query_row(
                    "SELECT source,duration,subtitle_source,created_at,input_ref FROM tasks WHERE id=?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .ok()?;
            let (transcript, summary) = get_note(app.clone(), id).ok()?;
            let metadata = format!(
                "# {title}\n\n> 来源：{}  \n{}> 时长：{}  \n> 字幕：{}  \n> 整理时间：{}\n",
                metadata.0,
                export_origin(&metadata.0, &metadata.4),
                metadata.1,
                metadata.2.unwrap_or_else(|| "未标记".into()),
                metadata.3
            );
            let content = match export_type.as_str() {
                "summaryAndTranscript" => {
                    format!("\n{}\n\n## 全文字幕\n\n{}\n", summary, transcript)
                }
                "summary" => format!("\n{}\n", summary),
                "transcript" => format!("\n## 全文字幕\n\n{}\n", transcript),
                _ => unreachable!("export type was validated above"),
            };
            Some(format!("{metadata}{content}"))
        })
        .unwrap_or_else(|| {
            format!("# {title}\n\n这是一份由快看视频 QuickVideo 生成的结构化视频笔记。\n")
        });
    fs::write(&path, body).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn transcribe_local_file(
    app: tauri::AppHandle,
    input_path: String,
    task_id: i64,
    registry: tauri::State<'_, TaskRegistry>,
) -> Result<String, String> {
    let registry = registry.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        transcribe_local_file_blocking(app, input_path, task_id, registry)
    })
    .await
    .map_err(|e| format!("后台转写任务中断：{e}"))?
}

fn transcribe_local_file_blocking(
    app: tauri::AppHandle,
    input_path: String,
    task_id: i64,
    registry: TaskRegistry,
) -> Result<String, String> {
    let work = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("work");
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let id = chrono::Utc::now().timestamp_millis();
    let wav = work.join(format!("{id}.wav"));
    let output = work.join(format!("{id}-transcript"));
    let mut ffmpeg_command = Command::new(tool_path("ffmpeg")?);
    let ffmpeg = run_task_command(
        &registry,
        task_id,
        ffmpeg_command
            .args([
                "-y",
                "-i",
                &input_path,
                "-ar",
                "16000",
                "-ac",
                "1",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&wav),
    )?;
    if !ffmpeg.status.success() {
        return Err(String::from_utf8_lossy(&ffmpeg.stderr).into_owned());
    }
    let model = base_model_path(&app)?;
    if !model.exists() {
        return Err("未找到本地语音模型，请在设置中下载模型。".into());
    }
    let language = whisper_language(&app);
    let mut whisper_command = Command::new(tool_path("whisper-cli")?);
    let whisper = run_task_command(
        &registry,
        task_id,
        whisper_command
            .args(["-m"])
            .arg(model)
            .args(["-f"])
            .arg(&wav)
            .args(["-l", &language, "-osrt", "-of"])
            .arg(&output),
    )?;
    if cleanup_temp_media(&app) {
        let _ = fs::remove_file(&wav);
    }
    if !whisper.status.success() {
        return Err(String::from_utf8_lossy(&whisper.stderr).into_owned());
    }
    let srt = output.with_extension("srt");
    let transcript = fs::read_to_string(&srt).map_err(|e| e.to_string())?;
    if cleanup_temp_media(&app) {
        let _ = fs::remove_file(srt);
    }
    Ok(transcript)
}

#[tauri::command]
async fn transcribe_platform_link(
    app: tauri::AppHandle,
    url: String,
    task_id: i64,
    registry: tauri::State<'_, TaskRegistry>,
) -> Result<String, String> {
    let registry = registry.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        transcribe_platform_link_blocking(app, url, task_id, registry)
    })
    .await
    .map_err(|e| format!("后台转写任务中断：{e}"))?
}

fn transcribe_platform_link_blocking(
    app: tauri::AppHandle,
    url: String,
    task_id: i64,
    registry: TaskRegistry,
) -> Result<String, String> {
    if !(url.contains("youtube.com") || url.contains("youtu.be") || url.contains("bilibili.com")) {
        return Err("目前仅支持 YouTube 与 Bilibili 链接。".into());
    }
    let work = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("work");
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let prefix = work.join(format!(
        "link-audio-{}",
        chrono::Utc::now().timestamp_millis()
    ));
    let mut download_command = ytdlp_command()?;
    let downloaded = run_task_command(
        &registry,
        task_id,
        download_command
            .args(["--no-playlist", "-x", "--audio-format", "wav", "-o"])
            .arg(format!("{}.%(ext)s", prefix.display()))
            .arg(&url),
    )?;
    if !downloaded.status.success() {
        return Err(format!(
            "无法取得可处理的临时音频：{}",
            String::from_utf8_lossy(&downloaded.stderr)
        ));
    }
    let audio_prefix = prefix
        .file_name()
        .ok_or_else(|| "无法创建临时音频文件。".to_string())?
        .to_string_lossy()
        .into_owned();
    let audio = fs::read_dir(&work)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&audio_prefix))
        });
    let audio = audio.ok_or_else(|| "未找到临时音频文件。".to_string())?;
    let output = work.join(format!(
        "link-transcript-{}",
        chrono::Utc::now().timestamp_millis()
    ));
    let model = base_model_path(&app)?;
    if !model.exists() {
        if cleanup_temp_media(&app) {
            let _ = fs::remove_file(&audio);
        }
        return Err("未找到本地语音模型，请在设置中下载模型。".into());
    }
    let mut whisper_command = Command::new(tool_path("whisper-cli")?);
    let whisper = run_task_command(
        &registry,
        task_id,
        whisper_command
            .args(["-m"])
            .arg(model)
            .args(["-f"])
            .arg(&audio)
            .args(["-l", &whisper_language(&app), "-osrt", "-of"])
            .arg(&output),
    )?;
    if cleanup_temp_media(&app) {
        let _ = fs::remove_file(&audio);
    }
    if !whisper.status.success() {
        return Err(String::from_utf8_lossy(&whisper.stderr).into_owned());
    }
    let srt = output.with_extension("srt");
    let transcript = fs::read_to_string(&srt).map_err(|e| e.to_string())?;
    if cleanup_temp_media(&app) {
        let _ = fs::remove_file(srt);
    }
    Ok(transcript)
}

fn read_platform_srt(
    app: &tauri::AppHandle,
    work: &std::path::Path,
    expected_prefix: &str,
) -> Result<Option<String>, String> {
    for entry in fs::read_dir(work).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "srt")
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(expected_prefix))
        {
            let transcript = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            if cleanup_temp_media(app) {
                let _ = fs::remove_file(path);
            }
            return Ok(Some(transcript));
        }
    }
    Ok(None)
}

#[tauri::command]
async fn fetch_platform_subtitles(
    app: tauri::AppHandle,
    url: String,
    task_id: i64,
    registry: tauri::State<'_, TaskRegistry>,
) -> Result<PlatformSubtitles, String> {
    let registry = registry.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        fetch_platform_subtitles_blocking(app, url, task_id, registry)
    })
    .await
    .map_err(|e| format!("后台字幕任务中断：{e}"))?
}

fn fetch_platform_subtitles_blocking(
    app: tauri::AppHandle,
    url: String,
    task_id: i64,
    registry: TaskRegistry,
) -> Result<PlatformSubtitles, String> {
    if !(url.contains("youtube.com") || url.contains("youtu.be") || url.contains("bilibili.com")) {
        return Err("目前仅支持 YouTube 与 Bilibili 链接。".into());
    }
    let work = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("work");
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let prefix = work.join(format!("link-{}", chrono::Utc::now().timestamp_millis()));
    let template = format!("{}.%(ext)s", prefix.display());
    let expected_prefix = prefix
        .file_name()
        .ok_or_else(|| "无法创建临时字幕文件。".to_string())?
        .to_string_lossy()
        .into_owned();
    let mut manual_command = ytdlp_command()?;
    let manual = run_task_command(
        &registry,
        task_id,
        manual_command
            .args([
                "--skip-download",
                "--write-subs",
                "--sub-langs",
                "zh.*,en.*",
                "--convert-subs",
                "srt",
                "-o",
            ])
            .arg(&template)
            .arg(&url),
    )?;
    if let Some(transcript) = read_platform_srt(&app, &work, &expected_prefix)? {
        return Ok(PlatformSubtitles {
            transcript,
            source: "平台人工字幕".into(),
        });
    }
    let mut automatic_command = ytdlp_command()?;
    let automatic = run_task_command(
        &registry,
        task_id,
        automatic_command
            .args([
                "--skip-download",
                "--write-auto-subs",
                "--sub-langs",
                "zh.*,en.*",
                "--convert-subs",
                "srt",
                "-o",
            ])
            .arg(&template)
            .arg(&url),
    )?;
    if let Some(transcript) = read_platform_srt(&app, &work, &expected_prefix)? {
        return Ok(PlatformSubtitles {
            transcript,
            source: "平台自动字幕".into(),
        });
    }
    Err(format!(
        "未发现可用字幕。{}{}",
        String::from_utf8_lossy(&manual.stderr),
        String::from_utf8_lossy(&automatic.stderr)
    ))
}

#[cfg(test)]
mod tests {
    use super::{export_origin, format_duration, transcript_chunks};

    #[test]
    fn formats_media_duration() {
        assert_eq!(format_duration(1.0), "0:01");
        assert_eq!(format_duration(62.0), "1:02");
        assert_eq!(format_duration(3661.0), "1:01:01");
    }

    #[test]
    fn splits_transcript_on_line_boundaries() {
        let transcript = "00:00 first\n00:01 second\n00:02 third\n";
        let chunks = transcript_chunks(transcript, 25);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].contains("first"));
        assert!(chunks[1].contains("third"));
    }

    #[test]
    fn exports_the_original_link_or_local_filename() {
        assert_eq!(
            export_origin("YouTube 链接", "https://example.com/watch?v=123"),
            "> 原始链接：<https://example.com/watch?v=123>  \n"
        );
        assert_eq!(
            export_origin("本地文件 · whisper.cpp", "/Users/me/Videos/lecture.mp4"),
            "> 本地文件：lecture.mp4  \n"
        );
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(TaskRegistry::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            check_runtime,
            download_base_model,
            get_local_duration,
            get_link_metadata,
            list_tasks,
            recover_incomplete_tasks,
            create_task,
            update_task,
            update_task_metadata,
            activate_task,
            cancel_task,
            release_task,
            clear_tasks,
            save_note,
            get_note,
            export_markdown,
            transcribe_local_file,
            transcribe_platform_link,
            fetch_platform_subtitles,
            save_model_config,
            get_model_config,
            get_preferences,
            save_preferences,
            generate_summary,
            test_model_connection,
            list_api_calls
        ])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
