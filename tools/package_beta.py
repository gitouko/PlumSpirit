#!/usr/bin/env python3
"""B1-0191: builds the v1.0-beta2 portable playtest package from a built EXE.

python3 tools/package_beta.py EXE SF2 OUT_DIR [BUILD_COMMIT]

EXE      the Windows PlumSpirit.exe (target/release/PlumSpirit.exe)
SF2      GeneralUser-GS.sf2 v2.0.3 (32,319,396 bytes), or "-" to leave it out
OUT_DIR  receives:
  PlumSpirit v1.0-beta2 (Windows x64)/        the portable folder
  PlumSpirit-v1.0-beta2-Windows-x64.zip       the folder without the SoundFont
  PlumSpirit-v1.0-beta2-SoundFont.zip         SoundFonts/GeneralUser-GS.sf2 at the same path
  PlumSpirit-source-v1.0-beta2.zip            `git archive` of HEAD (the source snapshot)
  package.json                                names, sizes and SHA-256 of everything\n\nSet PLUMSPIRIT_PE_READER to llvm-readobj and PLUMSPIRIT_RUNTIME_DLL to the\nredistributable x64 vcruntime140.dll. Existing portable folders are never removed.

BUILD_COMMIT is the commit the EXE was built from (default HEAD). The source snapshot is
HEAD; packaging refuses when anything the EXE is built from (crates/, assets/, Cargo.*)
differs between the two, so a records-only commit after the build keeps the EXE valid.

The two Windows zips stay under the 30 MB the desktop bridge accepts per file and
extract into the same folder. No private MIDI files or other SoundFonts are packaged.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
NAME = "PlumSpirit v1.0-beta2 (Windows x64)"
SF2_BYTES = 32_319_396
SF2_SHA = "9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe"
PLAYTEST = os.path.join(ROOT, "docs", "v1_0_beta2", "playtest")


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def git(*a):
    return subprocess.run(["git", "-C", ROOT, *a], check=True, capture_output=True, text=True).stdout.strip()


def pe_imports(exe):
    """Read the real PE import table, including native MSVC runtime dependencies."""
    reader = os.environ.get("PLUMSPIRIT_PE_READER", "llvm-readobj")
    out = subprocess.run([reader, "--coff-imports", exe], check=True,
                         capture_output=True, text=True).stdout
    names = {line.split(":", 1)[1].strip().lower()
             for line in out.splitlines() if line.strip().startswith("Name:")}
    if not names:
        raise SystemExit("PE import reader returned no imports")
    return sorted(names)


def crlf(text):
    return text.replace("\r\n", "\n").replace("\n", "\r\n")


def main(exe, sf2, out, built=None):
    head = git("rev-parse", "HEAD")
    built = git("rev-parse", built or "HEAD")
    if git("status", "--porcelain", "--untracked-files=no"):
        raise SystemExit("the working tree has uncommitted changes: commit before packaging")
    if git("diff", "--name-only", built, head, "--", "crates", "assets", "Cargo.toml", "Cargo.lock"):
        raise SystemExit(f"the EXE's sources changed since {built[:7]}: rebuild it from HEAD")
    folder = os.path.join(out, NAME)
    if os.path.exists(folder):
        raise SystemExit(f"output folder already exists: {folder}; choose a fresh output directory")
    os.makedirs(os.path.join(folder, "SoundFonts"))
    shutil.copy2(exe, os.path.join(folder, "PlumSpirit.exe"))
    for f in ("README.md", "QUICK_START.md", "KNOWN_ISSUES.md", "HOW_TO_REPORT.md"):
        shutil.copy2(os.path.join(PLAYTEST, f), os.path.join(folder, f))
    with open(os.path.join(PLAYTEST, "START HERE.txt"), encoding="utf-8") as f:
        open(os.path.join(folder, "START HERE.txt"), "w", encoding="utf-8", newline="").write(crlf(f.read()))
    for f in ("CHANGELOG.md", "THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.txt", "MICROSOFT_RUNTIME_NOTICE.txt"):
        shutil.copy2(os.path.join(ROOT, f), os.path.join(folder, f))
    shutil.copytree(os.path.join(ROOT, "crates", "plumspirit", "examples-lua"), os.path.join(folder, "examples-lua"))
    shutil.copy2(os.path.join(ROOT, "tools", "license-texts", "GeneralUser-GS-LICENSE.txt"),
                 os.path.join(folder, "SoundFonts", "GeneralUser-GS-LICENSE.txt"))
    if sf2 != "-":
        if os.path.getsize(sf2) != SF2_BYTES or sha(sf2) != SF2_SHA:
            raise SystemExit(f"{sf2} is not GeneralUser GS v2.0.3")
        shutil.copy2(sf2, os.path.join(folder, "SoundFonts", "GeneralUser-GS.sf2"))
    open(os.path.join(folder, "SoundFonts", "README.txt"), "w", encoding="utf-8", newline="").write(crlf(
        "GeneralUser GS v2.0.3 by S. Christian Collins\n\n"
        "PlumSpirit plays through GeneralUser-GS.sf2 in this folder when no other SoundFont\n"
        "is chosen. It comes in the separate PlumSpirit-v1.0-beta2-SoundFont.zip: extract that\n"
        "zip to the same place as the program zip (it fills this folder), or copy\n"
        "GeneralUser-GS.sf2 here from an earlier PlumSpirit folder. Any other .sf2 works too:\n"
        "Playback -> Choose SoundFont...\n\n"
        f"GeneralUser-GS.sf2: {SF2_BYTES:,} bytes; SHA-256:\n{SF2_SHA}\n\n"
        "The original author's license is GeneralUser-GS-LICENSE.txt in this folder.\n"
        "Project: https://github.com/mrbumpy409/GeneralUser-GS\n"))
    exe_sha = sha(os.path.join(folder, "PlumSpirit.exe"))
    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip()
    imports = pe_imports(exe)
    if "vcruntime140.dll" in imports:
        runtime = os.environ.get("PLUMSPIRIT_RUNTIME_DLL")
        if not runtime or not os.path.isfile(runtime):
            raise SystemExit("set PLUMSPIRIT_RUNTIME_DLL to the redistributable x64 vcruntime140.dll")
        shutil.copy2(runtime, os.path.join(folder, "vcruntime140.dll"))
    open(os.path.join(folder, "BUILD_INFO.txt"), "w", encoding="utf-8", newline="").write(crlf(
        "PlumSpirit v1.0-beta2 (1.0.0-beta.2) - Windows x64 playtest build\n\n"
        f"Built from commit: {built} (branch v1.0-beta2-dev)\n"
        + f"Toolchain: {rustc}; target x86_64-pc-windows-msvc; linker MSVC link.exe (native Visual Studio Build Tools)\n"
        "Profile: release (opt-level 3, thin LTO, 4 codegen units, symbols stripped, panic = unwind)\n"
        "Command: cargo build --locked --release --offline -p plumspirit --bin PlumSpirit\n"
        f"PlumSpirit.exe: {os.path.getsize(exe):,} bytes; SHA-256 {exe_sha}\n"
        f"PE imports (vcruntime140.dll bundled; other DLLs supplied by Windows): {', '.join(imports)}\n"
        "Code signing: none (Windows SmartScreen may warn on first start)\n"))
    files = {}
    for dp, _, fs in os.walk(folder):
        for f in fs:
            p = os.path.join(dp, f)
            files[os.path.relpath(p, out).replace(os.sep, "/")] = {"bytes": os.path.getsize(p), "sha256": sha(p)}
    zips = {}

    def make_zip(zname, members):
        zp = os.path.join(out, zname)
        if os.path.exists(zp):
            os.remove(zp)
        with zipfile.ZipFile(zp, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
            for rel in sorted(members):
                z.write(os.path.join(out, rel), rel)
        zips[zname] = {"bytes": os.path.getsize(zp), "sha256": sha(zp)}
        if os.path.getsize(zp) > 30 * 1000 * 1000:
            print(f"WARNING {zname} is {os.path.getsize(zp):,} bytes (over the bridge's 30 MB)", file=sys.stderr)

    sf_rel = f"{NAME}/SoundFonts/GeneralUser-GS.sf2"
    make_zip("PlumSpirit-v1.0-beta2-Windows-x64.zip", [r for r in files if r != sf_rel])
    if sf_rel in files:
        make_zip("PlumSpirit-v1.0-beta2-SoundFont.zip", [sf_rel])
    src = os.path.join(out, "PlumSpirit-source-v1.0-beta2.zip")
    subprocess.run(["git", "-C", ROOT, "archive", "--format=zip", "-9", "--prefix=PlumSpirit-source-v1.0-beta2/",
                    "-o", src, "HEAD"], check=True)
    zips["PlumSpirit-source-v1.0-beta2.zip"] = {"bytes": os.path.getsize(src), "sha256": sha(src)}
    info = {"version": "1.0.0-beta.2", "release": "PlumSpirit v1.0-beta2", "source_commit": head,
            "exe_built_from": built,
            "toolchain": rustc, "target": "x86_64-pc-windows-msvc", "exe_sha256": exe_sha,
            "pe_imports": imports, "runtime_dll_sha256": sha(os.path.join(folder, "vcruntime140.dll")), "source_license": "no license by owner request; inspection source snapshot only",
            "archives": zips, "folder": NAME, "files": files}
    json.dump(info, open(os.path.join(out, "package.json"), "w"), indent=1)
    print(json.dumps({"exe_sha256": exe_sha, "archives": zips, "imports": imports}, indent=1))


if __name__ == "__main__":
    if len(sys.argv) not in (4, 5):
        raise SystemExit(__doc__)
    main(*sys.argv[1:])
