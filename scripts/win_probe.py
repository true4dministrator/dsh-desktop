"""列出 dsh-desktop.exe 的顶层窗口（hwnd/标题/区域/是否可见），并可截图该窗口。

用法：
    python win_probe.py            # 只列出
    python win_probe.py shot out.png   # 置前并截图该窗口
"""
import ctypes
import sys
from ctypes import wintypes as wt

user32 = ctypes.windll.user32
kernel32 = ctypes.windll.kernel32
dwmapi = ctypes.windll.dwmapi
user32.SetProcessDPIAware()

PROC = "dsh-desktop.exe"


def proc_name(pid):
    h = kernel32.OpenProcess(0x1000, False, pid)
    if not h:
        return "?"
    buf = ctypes.create_unicode_buffer(512)
    size = wt.DWORD(512)
    ok = kernel32.QueryFullProcessImageNameW(h, 0, buf, ctypes.byref(size))
    kernel32.CloseHandle(h)
    return buf.value.split("\\")[-1] if ok else "?"


def targets():
    out = []

    @ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
    def cb(hwnd, _):
        pid = wt.DWORD()
        user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
        if proc_name(pid.value).lower() != PROC:
            return True
        # 含 DWM cloaked 状态：cloaked=1 说明窗口被系统隐藏（虚拟桌面/未显示）
        cloaked = wt.DWORD()
        dwmapi.DwmGetWindowAttribute(hwnd, 14, ctypes.byref(cloaked), ctypes.sizeof(cloaked))
        rect = wt.RECT()
        user32.GetWindowRect(hwnd, ctypes.byref(rect))
        title = ctypes.create_unicode_buffer(256)
        user32.GetWindowTextW(hwnd, title, 256)
        cls = ctypes.create_unicode_buffer(256)
        user32.GetClassNameW(hwnd, cls, 256)
        out.append({
            "hwnd": hwnd, "pid": pid.value, "title": title.value, "cls": cls.value,
            "visible": bool(user32.IsWindowVisible(hwnd)), "cloaked": cloaked.value,
            "rect": (rect.left, rect.top, rect.right, rect.bottom),
        })
        return True

    user32.EnumWindows(cb, 0)
    return out


rows = targets()
for r in rows:
    w, h = r["rect"][2] - r["rect"][0], r["rect"][3] - r["rect"][1]
    print(f"hwnd={r['hwnd']:#010x} pid={r['pid']} visible={r['visible']} cloaked={r['cloaked']} "
          f"size={w}x{h} at=({r['rect'][0]},{r['rect'][1]}) cls={r['cls']!r} title={r['title']!r}")

if len(sys.argv) > 2 and sys.argv[1] == "shot" and rows:
    main = max(rows, key=lambda r: (r["rect"][2] - r["rect"][0]) * (r["rect"][3] - r["rect"][1]))
    hwnd = main["hwnd"]
    user32.ShowWindow(hwnd, 9)                      # SW_RESTORE
    user32.SetWindowPos(hwnd, -1, 0, 0, 0, 0, 0x0001 | 0x0002 | 0x0040)  # topmost
    user32.SetForegroundWindow(hwnd)
    ctypes.windll.kernel32.Sleep(700)
    from PIL import ImageGrab
    l, t, r_, b = main["rect"]
    ImageGrab.grab(bbox=(l, t, r_, b), all_screens=True).save(sys.argv[2])
    print(f"\nshot saved: {sys.argv[2]} ({r_-l}x{b-t})")
