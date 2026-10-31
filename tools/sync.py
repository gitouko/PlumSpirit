#!/usr/bin/env python3
"""Write-back helper for cloud sessions (zero-loss protocol, docs/DEV_STATE_v0_8.md "Durability").

sync.py stage BASE_REV        -> copies files changed since BASE_REV (incl. untracked)
                                 to /mnt/user-data/outputs/sync-N/, writes
                                 /home/claude/ps/last_sync.json (commit list + paths)
sync.py verify                -> compares SHA-256 of the staged-back PC copies
                                 (/mnt/user-data/uploads/...) with the repo files

The bridge inserts a C2PA `caBX` chunk into PNGs (and provenance data into audio
files) it writes: commit PNGs in the form read back from the PC; keep test media
as base64 text.
"""
import hashlib, json, os, shutil, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEV = r"C:\Users\ouko_\Music\Ouko's BMC Programs\PlumSpirit ~ Black MIDI Editor\PlumSpirit-source\PlumSpirit-source-v0.7"
# Where staged-back PC files land: the bridge names the root after the
# connected folder (PlumSpirit-source, or the whole PlumSpirit folder).
BACKS = [
    "/mnt/user-data/uploads/PlumSpirit-source/PlumSpirit-source-v0.7",
    "/mnt/user-data/uploads/PlumSpirit ~ Black MIDI Editor/PlumSpirit-source/PlumSpirit-source-v0.7",
]
STATE = os.path.join(os.path.dirname(REPO), "last_sync.json")  # outside the repo


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def changed(base):
    out = subprocess.run(["git", "-C", REPO, "diff", "--name-status", base], capture_output=True, text=True, check=True).stdout
    files, deleted = [], []
    for line in out.splitlines():
        parts = line.split("\t")
        st = parts[0]
        if st.startswith("D"):
            deleted.append(parts[1])
        elif st.startswith("R"):
            deleted.append(parts[1]); files.append(parts[2])
        else:
            files.append(parts[-1])
    unt = subprocess.run(["git", "-C", REPO, "ls-files", "--others", "--exclude-standard"], capture_output=True, text=True, check=True).stdout
    files += [l for l in unt.splitlines() if l]
    return sorted(set(files)), deleted


def stage(base):
    files, deleted = changed(base)
    n = 1
    while os.path.exists(f"/mnt/user-data/outputs/sync-{n}"):
        n += 1
    root = f"/mnt/user-data/outputs/sync-{n}"
    commits = []
    for f in files:
        dst = os.path.join(root, f)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy2(os.path.join(REPO, f), dst)
        commits.append({"stagedPath": dst, "devicePath": DEV + "\\" + f.replace("/", "\\")})
    json.dump({"files": files, "deleted": deleted, "commits": commits}, open(STATE, "w"), indent=1)
    print(json.dumps(commits))
    print("FILES", len(files), "DELETED (bridge cannot delete):", deleted, file=sys.stderr)


def verify():
    st = json.load(open(STATE))
    bad = 0
    for f in st["files"]:
        cands = [os.path.join(r, f) for r in BACKS if os.path.exists(os.path.join(r, f))]
        # The newest staged copy (an older session may have used the other root).
        b = max(cands, key=os.path.getmtime) if cands else ""
        if not b or sha(b) != sha(os.path.join(REPO, f)):
            bad += 1
            print("MISMATCH", f)
    print(f"verified {len(st['files']) - bad}/{len(st['files'])}")
    print(json.dumps([c["devicePath"] for c in st["commits"]]))


if __name__ == "__main__":
    if sys.argv[1] == "stage":
        stage(sys.argv[2])
    else:
        verify()
