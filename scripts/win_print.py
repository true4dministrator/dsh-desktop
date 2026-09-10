"""用 PrintWindow 直接抓取指定标题窗口的渲染内容（不受 z-order / 屏幕外影响）。

用法：python win_print.py <窗口标题> <输出png>
"""
import ctypes
import sys
from ctypes import wintypes as wt

user32 = ctypes.windll.user32
gdi32 = ctypes.windll.gdi32
user32.SetProcessDPIAware()

TITLE = sys.argv[1] if len(sys.argv) > 1 else "DSH - DeepSeek Harness"
OUT = sys.argv[2] if len(sys.argv) > 2 else r"D:\zch-dsh-desktop\scripts\win_print.png"

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
    sys.exit(f"未找到标题为 {TITLE!r} 的窗口")

for hwnd in hits:
    rect = wt.RECT()
    user32.GetWindowRect(hwnd, ctypes.byref(rect))
    w, h = rect.right - rect.left, rect.bottom - rect.top
    if w <= 0 or h <= 0:
        print(f"hwnd={hwnd:#x} 尺寸为 0，跳过")
        continue

    hdc = user32.GetWindowDC(hwnd)
    memdc = gdi32.CreateCompatibleDC(hdc)
    bmp = gdi32.CreateCompatibleBitmap(hdc, w, h)
    gdi32.SelectObject(memdc, bmp)
    # PW_RENDERFULLCONTENT = 2 —— 抓 WebView2/Chromium 这类 DirectComposition 内容必须用这个
    ok = user32.PrintWindow(hwnd, memdc, 2)

    class BITMAPINFOHEADER(ctypes.Structure):
        _fields_ = [("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
                    ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
                    ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG),
                    ("biYPelsPerMeter", wt.LONG), ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD)]

    bi = BITMAPINFOHEADER()
    bi.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bi.biWidth, bi.biHeight = w, -h
    bi.biPlanes, bi.biBitCount, bi.biCompression = 1, 32, 0
    buf = ctypes.create_string_buffer(w * h * 4)
    got = gdi32.GetDIBits(memdc, bmp, 0, h, buf, ctypes.byref(bi), 0)

    from PIL import Image
    im = Image.frombuffer("RGBA", (w, h), buf, "raw", "BGRA", 0, 1).convert("RGB")
    out = OUT if len(hits) == 1 else OUT.replace(".png", f"_{hwnd:x}.png")
    im.save(out)
    print(f"hwnd={hwnd:#x} {w}x{h} PrintWindow={ok} GetDIBits行数={got} -> {out}")

    gdi32.DeleteObject(bmp)
    gdi32.DeleteDC(memdc)
    user32.ReleaseDC(hwnd, hdc)
