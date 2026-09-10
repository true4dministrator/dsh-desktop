"""创建 GitHub Release 并上传构建产物（api.github.com 可达、但 git push / 网页上传不便时用）。

用法：
    python scripts/publish_release.py <TOKEN> <TAG> [说明文件路径] [资产路径...]

例：
    python scripts/publish_release.py ghp_xxx v0.1.3 RELEASE_NOTES.md \
        "target-v013/release/bundle/nsis/DSH Desktop_0.1.3_x64-setup.exe" \
        target-v013/release/dsh-desktop.exe
"""
import json
import os
import sys
import time
import urllib.error
import urllib.request

OWNER = "true4dministrator"
REPO = "dsh-desktop"
API = f"https://api.github.com/repos/{OWNER}/{REPO}"


def req(url, method="GET", payload=None, raw=None, ctype="application/json", timeout=300):
    data = raw if raw is not None else (json.dumps(payload).encode() if payload is not None else None)
    r = urllib.request.Request(url, data=data, method=method,
                              headers={"Authorization": f"Bearer {TOKEN}",
                                       "Accept": "application/vnd.github+json",
                                       "Content-Type": ctype})
    try:
        with urllib.request.urlopen(r, timeout=timeout) as resp:
            body = resp.read()
            return resp.status, (json.loads(body) if body else None)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:400]


TOKEN = sys.argv[1] if len(sys.argv) > 1 else ""
TAG = sys.argv[2] if len(sys.argv) > 2 else ""
NOTES = sys.argv[3] if len(sys.argv) > 3 else ""
ASSETS = sys.argv[4:]

if not TOKEN or not TAG:
    sys.exit(__doc__)

# 已存在同名 release 则复用（便于补传资产）
status, rel = req(f"{API}/releases/tags/{TAG}")
if status == 200:
    print(f"release {TAG} 已存在，复用 id={rel['id']}")
else:
    body = os.path.isfile(NOTES) and open(NOTES, encoding="utf-8").read() or f"Release {TAG}"
    status, rel = req(f"{API}/releases", "POST",
                      {"tag_name": TAG, "name": TAG, "body": body, "target_commitish": "main"})
    if status not in (200, 201):
        sys.exit(f"创建 release 失败 {status}: {rel}")
    print(f"已创建 release {TAG} id={rel['id']}")

status, existing = req(f"{API}/releases/{rel['id']}/assets")
have = {a["name"] for a in (existing or [])}

for path in ASSETS:
    if not os.path.isfile(path):
        print(f"  跳过（文件不存在）{path}")
        continue
    name = os.path.basename(path).replace(" ", ".")
    if name in have:
        print(f"  已存在，跳过 {name}")
        continue
    size = os.path.getsize(path)
    print(f"  上传 {name} ({size/1048576:.2f} MB)…")
    with open(path, "rb") as f:
        raw = f.read()
    t0 = time.time()
    status, res = req(f"https://uploads.github.com/repos/{OWNER}/{REPO}/releases/{rel['id']}/assets?name={name}",
                      "POST", raw=raw, ctype="application/octet-stream")
    if status in (200, 201):
        print(f"    ok  {res['browser_download_url']}  ({time.time()-t0:.0f}s)")
    else:
        print(f"    FAIL {status} {res}")

print(f"\n完成：https://github.com/{OWNER}/{REPO}/releases/tag/{TAG}")
