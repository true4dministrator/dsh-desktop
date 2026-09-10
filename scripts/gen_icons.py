"""从指定图片裁剪生成 DSH Desktop 应用图标。"""
from PIL import Image
import os
import sys

BASE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ICONS = os.path.join(BASE, "src-tauri", "icons")
os.makedirs(ICONS, exist_ok=True)

src = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser(r"C:\Users\zch\Desktop\deepseek.jpg")
print(f"source: {src}")
img = Image.open(src).convert("RGBA")
print(f"original size: {img.size}")

# 居中裁剪成正方形（取短边为边长）
w, h = img.size
side = min(w, h)
left = (w - side) // 2
top = (h - side) // 2
square = img.crop((left, top, left + side, top + side))
print(f"square cropped: {square.size}")

# 缩放到各尺寸
def save_png(im, path, size):
    im.resize((size, size), Image.LANCZOS).save(path, "PNG")
    print(f"  wrote {path} ({size}x{size})")

save_png(square, os.path.join(ICONS, "icon.png"), 512)
save_png(square, os.path.join(ICONS, "32x32.png"), 32)
save_png(square, os.path.join(ICONS, "128x128.png"), 128)
save_png(square, os.path.join(ICONS, "128x128@2x.png"), 256)
save_png(square, os.path.join(ICONS, "tray.png"), 32)

# ICO：多尺寸嵌入
square.resize((256, 256), Image.LANCZOS).save(
    os.path.join(ICONS, "icon.ico"),
    sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
print(f"  wrote {os.path.join(ICONS, 'icon.ico')} (multi-size)")

print("done. icons in:", ICONS)
