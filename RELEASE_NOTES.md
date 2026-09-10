## 修复

**1. 换一台电脑打开显示 `dsh web authentication required`（v0.1.3 没修干净）**

v0.1.3 已经开始捕获 dsh 启动时打印的 token URL，但**取早了**：它在端口就绪后只等固定 600ms 就去读，而实测这行 stdout 输出要晚 1~2 秒才到——于是又退回访问裸 `/`，继续撞 401。

本版改成轮询等待（最多 8 秒），拿到 token URL 再导航。同一台机器上刚跑通的日志：

```
21:48:02  port is ready
21:48:03  captured auth url from stdout: http://127.0.0.1:3080/?token=...
21:48:03  auth url: http://127.0.0.1:3080/?token=...
```

**2. 宿主环境里的 `NODE_OPTIONS` 会把 dsh 弄崩**

如果本程序是从别的工具链里被拉起来的，`NODE_OPTIONS` 会带着宿主的 preload 钩子一起传给 dsh 的 node 进程。那类钩子会改写文件删除语义，直接把 dsh 的原子写锁搞崩：

```
[safe-delete] 操作失败: ERROR C:\Users\zch\.dsh\.credentials.yaml.lock
  → failed to apply loader entry connection (@deepseek-ai/dsh-client-connection)
```

现在启动 dsh / npm 时都显式清掉 `NODE_OPTIONS` 和 `NODE_REPL_EXTERNAL_MODULE`。

**3. 加了启动器自己的日志（排障用）**

正式包是无控制台的 GUI 程序，之前 `eprintln!` 写的诊断信息在用户机器上完全看不到，出问题只能靠猜。现在关键节点都落到：

```
%APPDATA%\dsh-desktop\launcher.log
```

记录版本、exe 路径、环境检测、spawn、端口就绪、捕获到的 token、导航目标等。

## 安装

下载 `DSH.Desktop_0.1.4_x64-setup.exe` 安装，或直接用免安装的 `dsh-desktop.exe`。

需要本机已有 Node.js 与 `dsh` CLI（`npm i -g @deepseek-ai/dsh`）。
