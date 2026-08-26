# DSH Desktop

把 DeepSeek Harness CLI（`dsh`）变成**双击即用的桌面应用**——不用再打开命令行跑 `dsh web` 再手动开浏览器。基于 **Tauri 2** 构建，Windows 原生体验，主程序仅 **3.6 MB**。

## 特性

- **双击即用**：自动拉起 `dsh web` 服务，秒开 DSH 界面
- **单实例**：重复双击 exe / 快捷方式只会唤起已有的隐藏窗口，不会开第二个进程、出现两个托盘图标
- **常驻后台**：关闭窗口自动隐藏到系统托盘，dsh 服务继续运行，下次启动秒开
- **一键安装**：未安装 dsh / pnpm / npm 时，引导页一键补齐（`npm install -g @deepseek-ai/dsh`、`pnpm`、npm 升级）
- **自动更新检测**：启动时后台对比 npm 最新版本，有新版时开屏页横幅提示，一键升级
- **服务管理**：托盘菜单提供「显示窗口 / 检查 dsh 更新 / 退出（保留服务）/ 退出并停止服务」

## 安装

### 方式一：安装包（推荐）

下载 `DSH Desktop_0.1.0_x64-setup.exe`，双击安装，桌面会出现「DSH Desktop」图标。

### 方式二：免安装版

下载 `dsh-desktop.exe`，双击直接运行。

> 首次运行若本机未安装 dsh CLI（或缺少 pnpm/npm），应用会自动弹出引导页一键安装。需要本机已安装 [Node.js](https://nodejs.org)。

## 从源码构建

### 环境要求

- Rust（MSVC 工具链，`rustup target add x86_64-pc-windows-msvc`）
- Node.js 18+
- WebView2（Windows 10/11 自带）

### 构建

```bash
# 安装前端依赖（Tauri CLI）
npm install

# 开发模式编译
cd src-tauri
cargo build

# 打包 NSIS 安装程序
cd ..
npx tauri build --bundles nsis
```

### 产物

| 文件 | 说明 |
|---|---|
| `src-tauri/target/release/dsh-desktop.exe` | 免安装版主程序（3.6 MB） |
| `src-tauri/target/release/bundle/nsis/DSH Desktop_0.1.0_x64-setup.exe` | NSIS 安装包 |

## 使用说明

- **启动**：双击图标 → 开屏 → 进入 DSH 界面
- **隐藏**：关闭窗口 → 自动隐藏到托盘（dsh 服务不停止）
- **唤起**：再次双击图标 / 点击托盘图标 → 立即回到界面
- **彻底退出**：托盘右键 → 「退出并停止服务」（同时停掉 dsh 进程）
- **日志**：`%APPDATA%\dsh-desktop\dsh.log`

## 工作原理

1. `dsh` 本质是一个本地 Web 服务（默认监听 `127.0.0.1:3080`）
2. 启动器检测 3080 端口：**已运行则直接复用**，未运行则后台拉起 `dsh web --no-open`（隐藏窗口、日志落盘）
3. 等待服务就绪后，WebView2 窗口加载 `http://localhost:3080` 承载完整 DSH 界面
4. 所有会话、配置、插件均保留在 `~/.dsh`（与命令行使用完全一致）

## 许可证

MIT
