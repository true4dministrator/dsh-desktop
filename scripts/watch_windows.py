"""监控新出现的可见窗口，抓取"左上角黑窗口"的真身。

用法：python watch_windows.py <minutes>
后台运行，每 2 秒枚举一次顶层可见窗口，记录"基线后新出现"的窗口
（hwnd 从未出现过且可见），写入 watch_windows.log。
"""
import ctypes
import datetime
import sys
import time
from ctypes import wintypes as wt

user32 = ctypes.windll.user32
kernel32 = ctypes.windll.kernel32

LOG = r"D:\zch-dsh-desktop\scripts\watch_windows.log"


def proc_name(pid):
    h = kernel32.OpenProcess(0x1000, False, pid)
    if not h:
        return "?"
    buf = ctypes.create_unicode_buffer(512)
    size = wt.DWORD(512)
    ok = kernel32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size))
    kernel32.CloseHandle(h)
    return buf.value.split("\\")[-1] if ok else "?"


def snapshot():
    """返回 {(hwnd): (pid, class, title, x, y, w, h, proc)}"""
    out = {}

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def cb(hwnd, lparam):
        if not user32.IsWindowVisible(hwnd):
            return True
        pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        rect = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        title = ctypes.create_unicode_buffer(256)
        user32.GetWindowTextW(hwnd, title, 256)
        cls = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, cls, 256)
        out[hwnd] = (
            pid.value,
            cls.value,
            title.value,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            proc_name(pid.value),
        )
        return True

    user32.EnumWindows(cb, 0)
    return out


def log(msg):
    line = "[%s] %s\n" % (
        datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
        msg,
    )
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(line)


def main():
    minutes = int(sys.argv[1]) if len(sys.argv) > 1 else 30
    deadline = time.time() + minutes * 60
    log(f"=== watch_windows started, monitor {minutes} min, pid={kernel32.GetCurrentProcessId()} ===")

    baseline = set(snapshot().keys())
    log(f"baseline visible windows: {len(baseline)}")

    seen = set(baseline)
    while time.time() < deadline:
        snap = snapshot()
        for hwnd, info in snap.items():
            if hwnd not in seen:
                seen.add(hwnd)
                # 新出现的可见窗口
                pid, cls, title, x, y, w, h, proc = info
                log(
                    f"NEW-WINDOW hwnd=0x{hwnd:x} pid={pid} proc={proc} "
                    f"pos=({x},{y}) size={w}x{h} class={cls!r} title={title!r}"
                )
        # 记录消失的窗口（可选，先不记）
        time.sleep(2)

    log("=== watch_windows finished ===")


if __name__ == "__main__":
    main()
