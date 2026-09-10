"""把 dsh-desktop 主窗口挪到屏幕内、置前并截图（诊断 WebView 实际渲染内容）。

用法：python win_shot.py <输出png> [宽 高]
"""
import ctypes
import sys
from ctypes import wintypes as wt

user32 = ctypes.windll.user32
user32.SetProcessDPIAware()

OUT = sys.argv[1] if len(sys.argv) > 1 else r"D:\zch-dsh-desktop\scripts\win_shot.png"
W = int(sys.argv[2]) if len(sys.argv) > 2 else 1500
H = int(sys.argv[3]) if len(sys.argv) > 3 else 950
X, Y = 60, 60

rows = []


@ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
def cb(hwnd, _):
    title = ctypes.create_unicode_buffer(256)
    user32.GetWindowTextW(hwnd, title, 256)
    if title.value == "DSH - DeepSeek Harness":
        rows.append(hwnd)
    return True


user32.EnumWindows(cb, 0)
if not rows:
    sys.exit("找不到主窗口（标题 'DSH - DeepSeek Harness'）")

hwnd = rows[0]
user32.ShowWindow(hwnd, 9)                      # SW_RESTORE
user32.SetWindowPos(hwnd, -1, X, Y, W, H, 0x0040)   # SWP_SHOWWINDOW
user32.SetForegroundWindow(hwnd)
ctypes.windll.kernel32.Sleep(1200)

from PIL import ImageGrab
ImageGrab.grab(bbox=(X, Y, X + W, Y + H), all_screens=True).save(OUT)
print(f"saved {OUT} ({W}x{H}) from hwnd={hwnd:#x}")
