//! DSH Desktop - DeepSeek Harness 桌面启动器
//!
//! 职责：
//! 1. 启动并托管 `dsh web` 本地服务（常驻后台）
//! 2. 提供一个无边框感的原生窗口承载 DSH 的 Web UI
//! 3. 关窗即隐藏到托盘，进程不退出
//! 4. 托盘菜单可选择「保留服务」或「停止服务」后退出
//! 5. 首启检测 dsh CLI 是否安装，缺失则进入一键安装引导

use std::io::{self, BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const PORT: u16 = 3080;
const DSH_URL: &str = "http://localhost:3080";
const READY_TIMEOUT_SECS: u64 = 60;
const POLL_INTERVAL_MS: u64 = 300;
const PROBE_TIMEOUT_MS: u64 = 300;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const PAGE_LOADING: &str = "index.html";
const PAGE_INSTALL: &str = "install.html";

/// 全局应用状态：dsh 子进程句柄，退出/托盘菜单需要访问。
struct AppState {
    service_child: Mutex<Option<Child>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            service_child: Mutex::new(None),
        }
    }
}

// ───────────────────────── dsh 服务管理 ─────────────────────────

fn service_running(port: u16) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    TcpStream::connect_timeout(&addr, Duration::from_millis(PROBE_TIMEOUT_MS)).is_ok()
}

fn wait_ready(port: u16, timeout_secs: u64) -> bool {
    let start = Instant::now();
    let deadline = Duration::from_secs(timeout_secs);
    while start.elapsed() < deadline {
        if service_running(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
    service_running(port)
}

/// 检测 dsh CLI 是否在 PATH 中可用。
fn dsh_cli_available() -> bool {
    #[cfg(windows)]
    {
        // dsh 在 Windows 是 dsh.cmd，`where dsh` 会按 PATHEXT 匹配
        let out = Command::new("cmd")
            .args(["/C", "where", "dsh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match out {
            Ok(o) => o.status.success() && !o.stdout.is_empty(),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        Command::new("sh")
            .args(["-c", "command -v dsh"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

fn log_file_path() -> std::path::PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("dsh-desktop"));
    let dir = base.join("dsh-desktop");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("dsh.log")
}

#[cfg(windows)]
fn start_service() -> io::Result<Child> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path())?;
    let err = log.try_clone()?;

    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "dsh", "web", "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
}

#[cfg(not(windows))]
fn start_service() -> io::Result<Child> {
    Command::new("dsh")
        .args(["web", "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(windows)]
fn kill_service_tree(child: &mut Child) {
    let pid = child.id();
    let _ = Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = child.wait();
}

#[cfg(not(windows))]
fn kill_service_tree(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn stop_service(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut guard = state.service_child.lock().unwrap();
    if let Some(child) = guard.as_mut() {
        eprintln!("[dsh-desktop] stopping dsh service tree (pid={})", child.id());
        kill_service_tree(child);
    }
    *guard = None;
}

/// 启动 dsh 服务并等待 ready，navigate 到 DSH UI。
/// 已在跑则直接 navigate；启不动则交给日志。
fn start_dsh_and_open(app: &AppHandle) {
    // 自愈：启动前先清理可能残留的 task-board 锁（强杀 dsh 进程会留下）
    cleanup_stale_taskboard_lock();

    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        if boot_dsh_once(&h) {
            return;
        }

        // 自愈重试：再清一次锁 + 杀掉可能的残留子进程 + 重启
        eprintln!("[dsh-desktop] boot failed, self-healing (clear lock + retry)…");
        cleanup_stale_taskboard_lock();
        if let Some(mut child) = h.state::<AppState>().service_child.lock().unwrap().take() {
            kill_service_tree(&mut child);
        }
        boot_dsh_once(&h);
    });
}

/// 一次完整的"检测 → 启动 → 等待就绪 → 跳转"。
/// 返回是否成功就绪。
fn boot_dsh_once(h: &AppHandle) -> bool {
    if service_running(PORT) {
        eprintln!("[dsh-desktop] dsh already listening on port {}", PORT);
        navigate_to_dsh(h);
        return true;
    }

    match start_service() {
        Ok(child) => {
            eprintln!(
                "[dsh-desktop] dsh spawned (pid={}), waiting for ready…",
                child.id()
            );
            *h.state::<AppState>().service_child.lock().unwrap() = Some(child);
        }
        Err(e) => {
            eprintln!("[dsh-desktop] failed to spawn dsh: {}", e);
            return false;
        }
    }

    if wait_ready(PORT, READY_TIMEOUT_SECS) {
        eprintln!("[dsh-desktop] dsh ready, navigating to {}", DSH_URL);
        navigate_to_dsh(h);
        true
    } else {
        eprintln!(
            "[dsh-desktop] dsh did not become ready within {}s; check log at {:?}",
            READY_TIMEOUT_SECS,
            log_file_path()
        );
        false
    }
}

fn navigate_to_dsh(h: &AppHandle) {
    if let Some(w) = h.get_webview_window("main") {
        if let Ok(url) = DSH_URL.parse() {
            let _ = w.navigate(url);
        }
    }
}

// ───────────────────────── 自愈：task-board 锁清理 ─────────────────────────

/// dsh 的 task-board 插件在强杀进程时会留下 `~/.dsh/task-board/ledger-v2.lock`，
/// 导致后续 dsh 启动报 "ledger is already owned by process <pid>" 直接退出。
/// 这里检查锁内 PID 是否存活：已死则删除锁（自愈）。
fn cleanup_stale_taskboard_lock() {
    let lock = taskboard_lock_path();
    if !lock.exists() {
        return;
    }

    match std::fs::read_to_string(&lock) {
        Ok(content) => {
            match extract_pid(&content) {
                Some(pid) if pid_alive(pid) => {
                    eprintln!(
                        "[dsh-desktop] task-board lock held by live pid {}, keep",
                        pid
                    );
                    return;
                }
                Some(pid) => {
                    eprintln!(
                        "[dsh-desktop] task-board lock pid {} is dead, removing stale lock",
                        pid
                    );
                }
                None => {
                    eprintln!("[dsh-desktop] task-board lock unparseable, removing");
                }
            }
        }
        Err(e) => {
            eprintln!("[dsh-desktop] cannot read task-board lock ({}), removing", e);
        }
    }

    if std::fs::remove_file(&lock).is_ok() {
        eprintln!("[dsh-desktop] stale task-board lock removed");
    } else {
        eprintln!("[dsh-desktop] failed to remove task-board lock");
    }
}

fn taskboard_lock_path() -> std::path::PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".dsh").join("task-board").join("ledger-v2.lock")
}

/// 从锁文件内容中提取 PID（数字，且大于 100，避免匹配到版本号等小数字）。
fn extract_pid(content: &str) -> Option<u32> {
    content
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u32>().ok())
        .find(|p| *p > 100)
}

/// Windows：用 tasklist /FI 检查 PID 是否存活。
#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    let out = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid)])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_lowercase().contains(".exe"),
        Err(_) => false,
    }
}

#[cfg(not(windows))]
fn pid_alive(pid: u32) -> bool {
    // Unix：kill -0 探测
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ───────────────────────── Tauri Commands（前端 invoke） ─────────────────────────

#[derive(Serialize)]
struct CheckResult {
    installed: bool,
    reason: Option<String>,
    hint: Option<String>,
}

#[tauri::command]
fn check_dsh() -> CheckResult {
    let installed = dsh_cli_available();
    if installed {
        CheckResult {
            installed: true,
            reason: None,
            hint: None,
        }
    } else {
        CheckResult {
            installed: false,
            reason: Some("未在 PATH 中找到 dsh/dsh.cmd".to_string()),
            hint: Some("需要先安装 Node.js（https://nodejs.org），然后应用会自动安装 dsh".to_string()),
        }
    }
}

#[tauri::command]
fn open_dsh(app: AppHandle) -> Result<(), String> {
    start_dsh_and_open(&app);
    Ok(())
}

#[derive(Serialize)]
struct InstallResult {
    ok: bool,
    error: Option<String>,
}

#[tauri::command]
async fn install_dsh(app: AppHandle) -> Result<InstallResult, String> {
    // 探测 node 是否可用
    let node_check = Command::new("cmd")
        .args(["/C", "where", "node"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    if !matches!(node_check, Ok(ref o) if o.status.success() && !o.stdout.is_empty()) {
        return Ok(InstallResult {
            ok: false,
            error: Some("未检测到 node，请先安装 Node.js（https://nodejs.org）后重试".to_string()),
        });
    }

    let mut child = Command::new("cmd")
        .args(["/C", "npm", "install", "-g", "@deepseek-ai/dsh"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("无法启动 npm：{}", e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取 npm 输出".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取 npm 错误流".to_string())?;

    let app_out = app.clone();
    let h_out = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            let _ = app_out.emit("install-log", line);
        }
    });
    let app_err = app.clone();
    let h_err = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            let _ = app_err.emit("install-log", line);
        }
    });

    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = h_out.join();
    let _ = h_err.join();

    if status.success() {
        Ok(InstallResult { ok: true, error: None })
    } else {
        Ok(InstallResult {
            ok: false,
            error: Some(format!(
                "npm 退出码 {}（请看上方日志）",
                status.code().unwrap_or(-1)
            )),
        })
    }
}

// ───────────────────────── 环境检测与补齐（pnpm / npm） ─────────────────────────

/// 执行命令并捕获 stdout（Windows 走 cmd /C）。
#[cfg(windows)]
fn capture_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("cmd")
        .args(["/C", program])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

#[cfg(not(windows))]
fn capture_stdout(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

#[derive(Serialize)]
struct EnvInfo {
    dsh_installed: bool,
    dsh_version: Option<String>,
    pnpm_installed: bool,
    npm_version: Option<String>,
}

#[tauri::command]
fn check_env() -> EnvInfo {
    EnvInfo {
        dsh_installed: dsh_cli_available(),
        dsh_version: capture_stdout("dsh", &["--version"]),
        pnpm_installed: capture_stdout("where", &["pnpm"]).is_some(),
        npm_version: capture_stdout("npm", &["--version"]),
    }
}

/// 执行 `npm <pkg_args>` 并把 stdout/stderr 流式 emit 到 `env-log` 事件。
async fn run_npm_global(app: &AppHandle, pkg_args: &[&str]) -> Result<(), String> {
    let mut child = Command::new("cmd")
        .args(["/C", "npm"])
        .args(pkg_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("无法启动 npm：{}", e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "无法读取 npm 输出".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "无法读取 npm 错误流".to_string())?;

    let a1 = app.clone();
    let h1 = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = a1.emit("env-log", line);
        }
    });
    let a2 = app.clone();
    let h2 = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = a2.emit("env-log", line);
        }
    });

    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = h1.join();
    let _ = h2.join();

    if status.success() {
        Ok(())
    } else {
        Err(format!("npm 退出码 {}", status.code().unwrap_or(-1)))
    }
}

/// 一键补齐 Node 环境：安装 pnpm + 升级 npm 到最新。
#[tauri::command]
async fn setup_node_env(app: AppHandle) -> Result<InstallResult, String> {
    let _ = app.emit("env-log", ">>> 安装 pnpm ...");
    if let Err(e) = run_npm_global(&app, &["install", "-g", "pnpm"]).await {
        return Ok(InstallResult {
            ok: false,
            error: Some(format!("安装 pnpm 失败：{}", e)),
        });
    }
    let _ = app.emit("env-log", ">>> 升级 npm 到最新 ...");
    if let Err(e) = run_npm_global(&app, &["install", "-g", "npm@latest"]).await {
        return Ok(InstallResult {
            ok: false,
            error: Some(format!("升级 npm 失败：{}", e)),
        });
    }
    let _ = app.emit("env-log", ">>> 环境就绪 ✓");
    Ok(InstallResult { ok: true, error: None })
}

// ───────────────────────── dsh 更新检测 ─────────────────────────

/// 后台对比 npm 最新版与本地 dsh 版本，有新版则 emit `update-available`。
fn check_for_update(app: &AppHandle) {
    let h = app.clone();
    std::thread::spawn(move || {
        let latest = capture_stdout("npm", &["view", "@deepseek-ai/dsh", "version"]);
        let current = capture_stdout("dsh", &["--version"]);
        match (latest, current) {
            (Some(l), Some(c)) if !l.trim().is_empty() && l.trim() != c.trim() => {
                eprintln!(
                    "[dsh-desktop] update available: {} -> {}",
                    c.trim(),
                    l.trim()
                );
                let _ = h.emit(
                    "update-available",
                    serde_json::json!({ "latest": l.trim(), "current": c.trim() }),
                );
            }
            (Some(l), Some(_)) => {
                eprintln!("[dsh-desktop] dsh is up to date ({})", l.trim());
            }
            _ => {
                eprintln!("[dsh-desktop] update check skipped (no network or dsh missing)");
            }
        }
    });
}

// ───────────────────────── 窗口 ─────────────────────────

fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn create_main_window(app: &AppHandle, page: &str) -> tauri::Result<()> {
    if app.get_webview_window("main").is_some() {
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page.into()))
        .title("DSH - DeepSeek Harness")
        .inner_size(1280.0, 820.0)
        .min_inner_size(960.0, 640.0)
        .center()
        .visible(true)
        .build()?;

    // 关窗 → 隐藏到托盘
    let handle = app.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            if let Some(w) = handle.get_webview_window("main") {
                let _ = w.hide();
            }
        }
    });

    Ok(())
}

// ───────────────────────── 托盘 ─────────────────────────

fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let check_update =
        MenuItem::with_id(app, "check_update", "检查 dsh 更新", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let exit_keep =
        MenuItem::with_id(app, "exit_keep", "退出（保留服务）", true, None::<&str>)?;
    let exit_stop =
        MenuItem::with_id(app, "exit_stop", "退出并停止服务", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &check_update, &sep, &exit_keep, &exit_stop])?;

    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .tooltip("DSH Desktop - DeepSeek Harness")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "check_update" => {
                eprintln!("[dsh-desktop] manual update check triggered");
                check_for_update(app);
            }
            "exit_keep" => {
                eprintln!("[dsh-desktop] exiting, keeping dsh service alive");
                app.exit(0);
            }
            "exit_stop" => {
                stop_service(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

// ───────────────────────── 入口 ─────────────────────────

pub fn run() {
    let context = tauri::generate_context!();

    tauri::Builder::default()
        // 单实例：双击 exe / 快捷方式时唤起已有隐藏窗口，第二个进程自动退出
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            eprintln!("[dsh-desktop] second instance detected, showing existing window");
            show_main_window(app);
        }))
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            check_dsh,
            check_env,
            install_dsh,
            setup_node_env,
            open_dsh
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // 1. 检测 dsh CLI；根据结果选择加载引导页或正常 loading 页
            if dsh_cli_available() {
                eprintln!("[dsh-desktop] dsh CLI detected, loading main flow");
                if let Err(e) = create_main_window(&handle, PAGE_LOADING) {
                    eprintln!("[dsh-desktop] failed to create window: {}", e);
                }
                start_dsh_and_open(&handle);
            } else {
                eprintln!("[dsh-desktop] dsh CLI missing, showing install guide");
                if let Err(e) = create_main_window(&handle, PAGE_INSTALL) {
                    eprintln!("[dsh-desktop] failed to create install window: {}", e);
                }
            }

            // 2. 创建系统托盘（引导页也显示托盘，用户可以退出）
            if let Err(e) = create_tray(&handle) {
                eprintln!("[dsh-desktop] failed to create tray: {}", e);
            }

            // 3. 后台检测 dsh 是否有新版本（有则 emit update-available）
            check_for_update(&handle);

            Ok(())
        })
        .build(context)
        .expect("error while building tauri application")
        .run(|_app_handle, _event| {});
}
