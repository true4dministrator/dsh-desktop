"""通过 GitHub Git Data API 把本地 HEAD 推送到仓库（github.com 直连被墙时 git push 的替代方案）。

用法：
    python scripts/push_via_api.py <TOKEN> [分支名]

原理：读本地 HEAD 的 tree，用 blobs + trees + commits + refs 一次性建一个提交，
等价于一次 git push，且不会污染提交历史（逐文件 PUT 会产生一堆碎片提交）。
"""
import base64
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request

TOKEN = sys.argv[1] if len(sys.argv) > 1 else ""
BRANCH = sys.argv[2] if len(sys.argv) > 2 else "main"
OWNER = "true4dministrator"
REPO = "dsh-desktop"
API = f"https://api.github.com/repos/{OWNER}/{REPO}"

# 不推上仓库的路径（构建产物、调试截图等）
EXCLUDE_DIRS = {"node_modules", "src-tauri/target", "target-v013", "dist_tmp", ".verify"}


def api(path, method="GET", payload=None):
    url = path if path.startswith("https://") else f"{API}{path}"
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {TOKEN}",
            "Accept": "application/vnd.github+json",
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            body = resp.read()
            return resp.status, (json.loads(body) if body else None)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:400]


def tracked_files():
    out = subprocess.check_output(["git", "ls-files"], text=True).splitlines()
    keep = []
    for p in out:
        norm = p.replace(os.sep, "/")
        if any(norm == d or norm.startswith(d + "/") for d in EXCLUDE_DIRS):
            continue
        if os.path.isfile(p):
            keep.append(norm)
    return keep


def main():
    files = tracked_files()
    print(f"本地已提交文件 {len(files)} 个，开始上传 blobs…")

    # 1) 先拿远端分支当前提交，取其 tree 作 base
    status, ref = api(f"/git/ref/heads/{BRANCH}")
    if status != 200:
        print(f"读取分支失败 {status}: {ref}")
        sys.exit(1)
    base_commit_sha = ref["object"]["sha"]
    status, base_commit = api(f"/git/commits/{base_commit_sha}")
    base_tree_sha = base_commit["tree"]["sha"]
    print(f"远端 base commit {base_commit_sha[:8]}")

    # 2) 逐个建 blob（内容寻址，重复内容不会重复占用）
    tree_entries = []
    for i, path in enumerate(files, 1):
        with open(path, "rb") as f:
            blob_b64 = base64.b64encode(f.read()).decode()
        status, blob = api("/git/blobs", "POST", {"content": blob_b64, "encoding": "base64"})
        if status not in (200, 201):
            print(f"  FAIL blob {path} {status} {blob}")
            sys.exit(1)
        tree_entries.append({"path": path, "mode": "100755" if os.access(path, os.X_OK) else "100644",
                             "type": "blob", "sha": blob["sha"]})
        if i % 20 == 0 or i == len(files):
            print(f"  已上传 {i}/{len(files)}")

    # 3) 建 tree（基于远端 base tree，仅覆盖有变化的路径）
    status, tree = api("/git/trees", "POST", {"base_tree": base_tree_sha, "tree": tree_entries})
    if status not in (200, 201):
        print(f"建 tree 失败 {status}: {tree}")
        sys.exit(1)

    # 4) 建提交并移动分支
    msg = subprocess.check_output(["git", "log", "-1", "--pretty=%s"], text=True).strip()
    status, commit = api("/git/commits", "POST",
                         {"message": msg, "tree": tree["sha"], "parents": [base_commit_sha]})
    if status not in (200, 201):
        print(f"建 commit 失败 {status}: {commit}")
        sys.exit(1)
    status, res = api(f"/git/refs/heads/{BRANCH}", "PATCH",
                      {"sha": commit["sha"], "force": False})
    if status not in (200, 201):
        print(f"更新分支失败 {status}: {res}")
        sys.exit(1)

    print(f"\n推送完成：{BRANCH} -> {commit['sha'][:8]}  ({len(files)} 个文件)")


if __name__ == "__main__":
    main()
