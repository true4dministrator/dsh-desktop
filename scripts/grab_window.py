"""把目标窗口挪到屏幕空白区、置顶并抓屏（用于看 WebView 真实渲染内容）。

用法：python grab_window.py <窗口标题> <输出png> [x y w h]
"""
import ctypes
import sys
from ctypes import wintypes as wt

user32 = ctypes.windll.user32
user32.SetProcessDPIAware()

TITLE = sys.argv[1] if len(sys.argv) > 1 else "DSH - DeepSeek Harness"
OUT = sys.argv[2] if len(sys.argv) > 2 else r"D:\zch-dsh-desktop\scripts\grab.png"
X = int(sys.argv[3]) if len(sys.argv) > 3 else 1080
Y = int(sys.argv[4]) if len(sys.argv) > 4 else 60
W = int(sys.argv[5]) if len(sys.argv) > 5 else 1400
H = int(sys.argv[6]) if len(sys.argv) > 6 else 1100

user32.SetWindowPos.argtypes = [wt.HWND, wt.HWND, ctypes.c_int, ctypes.c_int,
                               ctypes.c_int, ctypes.c_int, wt.UINT]
HWND_TOPMOST = wt.HWND(-1)
SWP_NOACTIVATE = 0x0010
SWP_SHOWWINDOW = 0x0040

hits = []


@ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)
def cb(hwnd, _):
    buf = ctypes.create_unicode_buffer(256)
    user32.GetWindowTextW(hwnd, buf, 256)
    if buf.value == TITLE:
        hits.append(hwnd)
    return True


user32.EnumWindows(cb, 0)
if not hits:
    sys.exit(f"未找到 {TITLE!r}")

hwnd = hits[0]
user32.ShowWindow(hwnd, 4)  # SW_SHOWNOACTIVATE：显示但不抢焦点
for _ in range(2):          # 置顶常需连设两次才生效
    user32.SetWindowPos(hwnd, HWND_TOPMOST, X, Y, W, H, SWP_NOACTIVATE | SWP_SHOWWINDOW)
ctypes.windll.kernel32.Sleep(1500)

from PIL import ImageGrab
ImageGrab.grab(bbox=(X, Y, X + W, Y + H), all_screens=True).save(OUT)
print(f"saved {OUT} region=({X},{Y},{W}x{H}) hwnd={hwnd:#x}")
