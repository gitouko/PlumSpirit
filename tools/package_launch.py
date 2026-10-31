#!/usr/bin/env python3
"""L-K01 / L-K04 / L-K05: builds the v1.0-Launch portable Windows package from a built EXE.

python3 tools/package_launch.py EXE SF2 OUT_DIR [BUILD_COMMIT]

EXE      the Windows PlumSpirit.exe (cross-built for x86_64-pc-windows-gnullvm, or native MSVC)
SF2      GeneralUser-GS.sf2 v2.0.3 (32,319,396 bytes), or "-" to leave it out
OUT_DIR  receives:
  PlumSpirit v1.0-Launch (Windows x64)/        the portable folder (one user document:
                                              README.md; docs/ and legal/ hold the rest)
  PlumSpirit-v1.0-Launch-Windows-x64.zip       the folder without the SoundFont
  PlumSpirit-v1.0-Launch-SoundFont.zip         SoundFonts/GeneralUser-GS.sf2 at the same path
  PlumSpirit-source-v1.0-Launch.zip            `git archive` of HEAD (the source snapshot)
  package.json                                names, sizes and SHA-256 of everything

Environment: PLUMSPIRIT_PE_READER (default llvm-readobj), PLUMSPIRIT_TOOLCHAIN (the
target / linker line of BUILD_INFO), PLUMSPIRIT_BUILD_COMMAND.

BUILD_COMMIT is the commit the EXE was built from (default HEAD); packaging refuses when
anything the EXE is built from (crates/, assets/, Cargo.*) differs between it and HEAD, so a
records-only commit after the build keeps the EXE valid. Every zip stays under the 30 MB the
desktop bridge accepts per file. No private MIDI files or other SoundFonts are packaged.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RELEASE = "PlumSpirit v1.0-Launch"
NAME = f"{RELEASE} (Windows x64)"
STEM = "PlumSpirit-v1.0-Launch"
SF2_BYTES = 32_319_396
SF2_SHA = "9575028c7a1f589f5770fccc8cff2734566af40cd26ed836944e9a5152688cfe"
PLAYTEST = os.path.join(ROOT, "docs", "v1_0_launch", "playtest")


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def git(*a):
    return subprocess.run(["git", "-C", ROOT, *a], check=True, capture_output=True, text=True).stdout.strip()


def pe_imports(exe):
    reader = os.environ.get("PLUMSPIRIT_PE_READER", "llvm-readobj")
    out = subprocess.run([reader, "--coff-imports", exe], check=True, capture_output=True, text=True).stdout
    names = {line.split(":", 1)[1].strip().lower()
             for line in out.splitlines() if line.strip().startswith("Name:")}
    if not names:
        raise SystemExit("PE import reader returned no imports")
    return sorted(names)


def crlf(text):
    return text.replace("\r\n", "\n").replace("\n", "\r\n")


def write_text(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    open(path, "w", encoding="utf-8", newline="").write(crlf(text))


def version():
    for line in open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8"):
        if line.startswith("version"):
            return line.split("=", 1)[1].strip().strip('"')
    raise SystemExit("no workspace version")


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
    ver = version()
    os.makedirs(os.path.join(folder, "SoundFonts"))
    shutil.copy2(exe, os.path.join(folder, "PlumSpirit.exe"))
    # L-K01: one user document at the top; everything else in docs\ and legal\.
    write_text(os.path.join(folder, "README.md"), open(os.path.join(PLAYTEST, "README.md"), encoding="utf-8").read())
    write_text(os.path.join(folder, "docs", "KNOWN_ISSUES.md"),
               open(os.path.join(PLAYTEST, "KNOWN_ISSUES.md"), encoding="utf-8").read())
    write_text(os.path.join(folder, "docs", "CHANGELOG.md"),
               open(os.path.join(ROOT, "CHANGELOG.md"), encoding="utf-8").read())
    os.makedirs(os.path.join(folder, "legal"))
    for f in ("THIRD_PARTY_NOTICES.md", "THIRD_PARTY_LICENSES.txt"):
        shutil.copy2(os.path.join(ROOT, f), os.path.join(folder, "legal", f))
    shutil.copytree(os.path.join(ROOT, "crates", "plumspirit", "examples-lua"), os.path.join(folder, "examples-lua"))
    shutil.copy2(os.path.join(ROOT, "tools", "license-texts", "GeneralUser-GS-LICENSE.txt"),
                 os.path.join(folder, "SoundFonts", "GeneralUser-GS-LICENSE.txt"))
    if sf2 != "-":
        if os.path.getsize(sf2) != SF2_BYTES or sha(sf2) != SF2_SHA:
            raise SystemExit(f"{sf2} is not GeneralUser GS v2.0.3")
        shutil.copy2(sf2, os.path.join(folder, "SoundFonts", "GeneralUser-GS.sf2"))
    write_text(os.path.join(folder, "SoundFonts", "README.txt"),
               "GeneralUser GS v2.0.3 by S. Christian Collins\n\n"
               "PlumSpirit plays through GeneralUser-GS.sf2 in this folder when no other SoundFont\n"
               f"is chosen. It comes in the separate {STEM}-SoundFont.zip: extract that zip to\n"
               "the same place as the program zip (it fills this folder), or copy GeneralUser-GS.sf2\n"
               "here from an earlier PlumSpirit folder. Any other .sf2 works too:\n"
               "Playback -> Choose SoundFont...\n\n"
               f"GeneralUser-GS.sf2: {SF2_BYTES:,} bytes; SHA-256:\n{SF2_SHA}\n\n"
               "The original author's license is GeneralUser-GS-LICENSE.txt in this folder.\n"
               "Project: https://github.com/mrbumpy409/GeneralUser-GS\n")
    exe_sha = sha(os.path.join(folder, "PlumSpirit.exe"))
    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip()
    imports = pe_imports(exe)
    toolchain = os.environ.get(
        "PLUMSPIRIT_TOOLCHAIN",
        "target x86_64-pc-windows-gnullvm; llvm-mingw 20261020 (UCRT) clang / lld; "
        "-C target-feature=+crt-static (libunwind, compiler-rt and the Rust standard library "
        "linked in; the C runtime is Windows' Universal CRT)")
    command = os.environ.get(
        "PLUMSPIRIT_BUILD_COMMAND",
        "cargo build --release --offline --target x86_64-pc-windows-gnullvm -p plumspirit")
    write_text(os.path.join(folder, "docs", "BUILD_INFO.txt"),
               f"{RELEASE} ({ver}) - Windows x64\n\n"
               f"Built from commit: {built} (branch v1.0-launch-dev)\n"
               f"Toolchain: {rustc}; {toolchain}\n"
               "Profile: release (opt-level 3, thin LTO, 4 codegen units, symbols stripped, panic = unwind)\n"
               f"Command: {command}\n"
               f"PlumSpirit.exe: {os.path.getsize(exe):,} bytes; SHA-256 {exe_sha}\n"
               f"PE imports (all supplied by Windows 10 / 11): {', '.join(imports)}\n"
               "Code signing: none (Windows SmartScreen may warn on first start)\n")
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
    make_zip(f"{STEM}-Windows-x64.zip", [r for r in files if r != sf_rel])
    if sf_rel in files:
        make_zip(f"{STEM}-SoundFont.zip", [sf_rel])
    src_name = "PlumSpirit-source-v1.0-Launch"
    src = os.path.join(out, f"{src_name}.zip")
    subprocess.run(["git", "-C", ROOT, "archive", "--format=zip", "-9", f"--prefix={src_name}/",
                    "-o", src, "HEAD"], check=True)
    zips[f"{src_name}.zip"] = {"bytes": os.path.getsize(src), "sha256": sha(src)}
    info = {"version": ver, "release": RELEASE, "source_commit": head, "exe_built_from": built,
            "toolchain": f"{rustc}; {toolchain}", "build_command": command, "exe_sha256": exe_sha,
            "pe_imports": imports,
            "source_license": "no license by owner request; inspection source snapshot only",
            "archives": zips, "folder": NAME, "files": files}
    json.dump(info, open(os.path.join(out, "package.json"), "w"), indent=1)
    print(json.dumps({"exe_sha256": exe_sha, "archives": zips, "imports": imports}, indent=1))


if __name__ == "__main__":
    if len(sys.argv) not in (4, 5):
        raise SystemExit(__doc__)
    main(*sys.argv[1:])
