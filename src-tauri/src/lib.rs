//! DSH Desktop - DeepSeek Harness 桌面启动器
//!
//! 职责：
//! 1. 启动并托管 `dsh web` 本地服务（常驻后台）
//! 2. 提供一个无边框感的原生窗口承载 DSH 的 Web UI
//! 3. 关窗即隐藏到托盘，进程不退出
//! 4. 托盘菜单可选择「保留服务」或「停止服务」后退出
//! 5. 首启检测 dsh CLI 是否安装，缺失则进入一键安装引导

use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
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
/// 等 dsh 在 stdout 打印带 token 认证 URL 的最长时间（秒）
const AUTH_WAIT_SECS: u64 = 8;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const PAGE_LOADING: &str = "index.html";
const PAGE_INSTALL: &str = "install.html";

/// 全局应用状态：dsh 子进程句柄，退出/托盘菜单需要访问。
struct AppState {
    service_child: Mutex<Option<Child>>,
    /// dsh web 启动时打印的认证 URL（首次使用需浏览器会话认证，`/?token=` 形式）。
    auth_url: Arc<Mutex<Option<String>>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            service_child: Mutex::new(None),
            auth_url: Arc::new(Mutex::new(None)),
        }
    }
}

/// 从 dsh web 启动输出行中提取认证 URL（形如 `http://127.0.0.1:3080/?token=...`）。
fn extract_auth_url(line: &str) -> Option<String> {
    const MARK: &str = "dsh web: http://";
    let idx = line.find(MARK)?;
    let rest = &line[idx + MARK.len()..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == ')')
        .unwrap_or(rest.len());
    let url = format!("http://{}", &rest[..end]);
    url.contains("?token=").then_some(url)
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

fn launcher_log_path() -> std::path::PathBuf {
    log_file_path().with_file_name("launcher.log")
}

/// 记一条启动器事件到 `%APPDATA%\dsh-desktop\launcher.log`。
/// 正式包是 windows 子系统（无控制台），`eprintln!` 在用户机器上看不到，
/// 排障只能靠这个文件，所以关键节点都要写。
fn log_event(msg: impl AsRef<str>) {
    if let Ok(mut f) =
        std::fs::OpenOptions::new().create(true).append(true).open(launcher_log_path())
    {
        // 时间戳用 Unix 秒（std 无本地时间格式化，读取时再换算）
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{}] {}", secs, msg.as_ref());
    }
}

/// 启动 `dsh web`：stderr 落盘；stdout 用管道捕获——既追加写 dsh.log，
/// 又实时提取"首次使用需认证"的带 token URL（存入 auth_url 供 WebView 完成认证）。
#[cfg(windows)]
fn start_service(auth_url: Arc<Mutex<Option<String>>>) -> io::Result<Child> {
    let err_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file_path())?;

    let mut cmd = Command::new("cmd");
    cmd.args(["/C", "dsh", "web", "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(err_log))
        // 宿主（如从其它工具链里启动本程序）可能通过 NODE_OPTIONS 给 node 注入
        // preload 钩子，那类 shim 会改写 fs 删除语义，直接把 dsh 的原子写锁搞崩：
        //   [safe-delete] 操作失败 ... .credentials.yaml.lock
        // 清掉它，保证 dsh 拿到干净的环境。
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_REPL_EXTERNAL_MODULE")
        .creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn()?;
    log_event(format!("spawned: cmd /C dsh web --no-open (pid={})", child.id()));

    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            let mut log =
                std::fs::OpenOptions::new().create(true).append(true).open(log_file_path()).ok();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(l) = log.as_mut() {
                    let _ = writeln!(l, "{}", line);
                }
                if let Some(url) = extract_auth_url(&line) {
                    log_event(format!("captured auth url from stdout: {}", url));
                    if let Ok(mut guard) = auth_url.lock() {
                        *guard = Some(url);
                    }
                }
            }
            log_event("dsh stdout stream closed");
        });
    }
    Ok(child)
}

#[cfg(not(windows))]
fn start_service(auth_url: Arc<Mutex<Option<String>>>) -> io::Result<Child> {
    let mut cmd = Command::new("dsh");
    cmd.args(["web", "--no-open"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(url) = extract_auth_url(&line) {
                    if let Ok(mut guard) = auth_url.lock() {
                        *guard = Some(url);
                    }
                }
            }
        });
    }
    Ok(child)
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
        log_event(format!(
            "port {} already in use -> reusing external dsh (no token available this run)",
            PORT
        ));
        navigate_to_dsh(h, false);
        return true;
    }

    let auth_url = h.state::<AppState>().auth_url.clone();
    match start_service(auth_url) {
        Ok(child) => {
            eprintln!(
                "[dsh-desktop] dsh spawned (pid={}), waiting for ready…",
                child.id()
            );
            *h.state::<AppState>().service_child.lock().unwrap() = Some(child);
        }
        Err(e) => {
            eprintln!("[dsh-desktop] failed to spawn dsh: {}", e);
            log_event(format!("spawn failed: {}", e));
            return false;
        }
    }

    if wait_ready(PORT, READY_TIMEOUT_SECS) {
        eprintln!("[dsh-desktop] dsh ready, navigating…");
        log_event("port is ready");
        navigate_to_dsh(h, true);
        true
    } else {
        eprintln!(
            "[dsh-desktop] dsh did not become ready within {}s; check log at {:?}",
            READY_TIMEOUT_SECS,
            log_file_path()
        );
        log_event(format!("not ready within {}s", READY_TIMEOUT_SECS));
        false
    }
}

/// 跳转到 dsh UI。若本次启动 dsh 打印了认证 URL（首次使用需浏览器会话认证），
/// 则先访问带 token 的 URL（服务端种 cookie 并 303 到 /），否则直接访问根 URL。
/// 等待 stdout 读取线程抓到认证 URL（最多 `timeout`）。
/// dsh 打印这行发生在监听成功附近，时机不固定，用固定 sleep 容易落空。
fn wait_auth_url(h: &AppHandle, timeout: Duration) -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(u) = h.state::<AppState>().auth_url.lock().unwrap().clone() {
            return Some(u);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// 跳转到 dsh UI。
/// `expect_auth` 为真（服务是本次拉起的）时，先等 stdout 里那行带 token 的 URL；
/// 拿到就先访问它（服务端种 cookie 并 303 到 /）完成认证，否则直接访问根 URL。
fn navigate_to_dsh(h: &AppHandle, expect_auth: bool) {
    let captured = if expect_auth {
        wait_auth_url(h, Duration::from_secs(AUTH_WAIT_SECS))
    } else {
        h.state::<AppState>().auth_url.lock().unwrap().clone()
    };
    log_event(format!(
        "auth url: {}",
        match captured.as_deref() {
            Some(u) => u.to_string(),
            None => "none (navigating to bare /)".to_string(),
        }
    ));
    let url = captured.unwrap_or_else(|| DSH_URL.to_string());
    if let Some(w) = h.get_webview_window("main") {
        if let Ok(u) = url.parse() {
            let _ = w.navigate(u);
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

// ───────────────────────── 辅助窗口移出屏幕（修复左上角黑框） ─────────────────────────

/// 单实例插件的标记窗口（`<identifier>-siw`）和 tao 的事件窗口（`Tao Thread Event Target`）
/// 会以 14x14 的小黑框残留在屏幕 (0,0)。它们必须常驻（单实例检测/消息循环依赖），
/// 但可以把它们移到屏幕外（-32000,-32000，Windows 隐藏窗口的标准做法），
/// 功能不受影响（FindWindow/消息接收不依赖可见位置）。
#[cfg(windows)]
fn move_helper_windows_offscreen() {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> i32 {
        let target_pid = lparam as u32;
        let mut pid: u32 = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == target_pid {
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                GetWindowRect(hwnd, &mut rect);
                let w = rect.right - rect.left;
                let h = rect.bottom - rect.top;
                // 只处理小型辅助窗口（主窗口 1280x820 远大于此）
                if w > 0 && w <= 64 && h > 0 && h <= 64 {
                    SetWindowPos(
                        hwnd,
                        HWND_TOP,
                        -32000,
                        -32000,
                        w,
                        h,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
        }
        1
    }

    unsafe {
        EnumWindows(Some(callback), std::process::id() as LPARAM);
    }
}

#[cfg(not(windows))]
fn move_helper_windows_offscreen() {}

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

/// 执行 `npm <pkg_args>` 并把 stdout/stderr 流式 emit 到 `event`。
async fn run_npm_global(app: &AppHandle, pkg_args: &[&str], event: &str) -> Result<(), String> {
    log_event(format!("npm {}", pkg_args.join(" ")));
    let mut child = Command::new("cmd")
        .args(["/C", "npm"])
        .args(pkg_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_REPL_EXTERNAL_MODULE")
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
    let ev1 = event.to_string();
    let h1 = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = a1.emit(&ev1, line);
        }
    });
    let a2 = app.clone();
    let ev2 = event.to_string();
    let h2 = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = a2.emit(&ev2, line);
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
    if let Err(e) = run_npm_global(&app, &["install", "-g", "pnpm"], "env-log").await {
        return Ok(InstallResult {
            ok: false,
            error: Some(format!("安装 pnpm 失败：{}", e)),
        });
    }
    let _ = app.emit("env-log", ">>> 升级 npm 到最新 ...");
    if let Err(e) = run_npm_global(&app, &["install", "-g", "npm@latest"], "env-log").await {
        return Ok(InstallResult {
            ok: false,
            error: Some(format!("升级 npm 失败：{}", e)),
        });
    }
    let _ = app.emit("env-log", ">>> 环境就绪 ✓");
    Ok(InstallResult { ok: true, error: None })
}

/// 更新 dsh 到最新版：先停掉正在运行的 dsh 服务，再 `npm i -g @deepseek-ai/dsh@latest`。
/// 更新完成后由前端调用 open_dsh 重启服务。
#[tauri::command]
async fn update_dsh(app: AppHandle) -> Result<InstallResult, String> {
    log_event("update_dsh: stopping service then npm i -g @deepseek-ai/dsh@latest");
    stop_service(&app);
    let _ = app.emit("install-log", ">>> 正在升级 dsh 到最新版 ...");
    match run_npm_global(&app, &["install", "-g", "@deepseek-ai/dsh@latest"], "install-log").await
    {
        Ok(()) => {
            let _ = app.emit("install-log", ">>> dsh 升级完成 ✓");
            log_event("update_dsh: success");
            Ok(InstallResult { ok: true, error: None })
        }
        Err(e) => {
            let _ = app.emit("install-log", format!(">>> 升级失败：{}", e));
            log_event(format!("update_dsh: failed: {}", e));
            Ok(InstallResult {
                ok: false,
                error: Some(format!("dsh 升级失败：{}", e)),
            })
        }
    }
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
        // 禁用 WebView2 GPU 合成（软件渲染），修复 DWM 合成残影导致的
        // 屏幕左上角"黑窗口"（dsh 高频输出 + 窗口变换时的旧帧残影）
        .additional_browser_args("--disable-gpu")
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
            update_dsh,
            open_dsh
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            log_event(format!(
                "=== launcher start v{} ({}) ===",
                app.package_info().version,
                std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
            ));
            log_event(format!(
                "NODE_OPTIONS present: {}",
                std::env::var("NODE_OPTIONS").is_ok()
            ));

            // 1. 检测 dsh CLI；根据结果选择加载引导页或正常 loading 页
            if dsh_cli_available() {
                eprintln!("[dsh-desktop] dsh CLI detected, loading main flow");
                log_event("dsh CLI detected on PATH");
                if let Err(e) = create_main_window(&handle, PAGE_LOADING) {
                    eprintln!("[dsh-desktop] failed to create window: {}", e);
                    log_event(format!("create window failed: {}", e));
                }
                start_dsh_and_open(&handle);
            } else {
                eprintln!("[dsh-desktop] dsh CLI missing, showing install guide");
                log_event("dsh CLI NOT found on PATH -> install guide");
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
        .run(|_app_handle, event| {
            // 事件循环就绪后，把单实例标记窗/tao 事件窗移出屏幕，避免左上角黑框
            if let tauri::RunEvent::Ready = event {
                move_helper_windows_offscreen();
            }
        });
}
