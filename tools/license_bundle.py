#!/usr/bin/env python3
"""Generates the third-party license inventory and the full license texts shipped
with the Windows executable.

    python3 tools/license_bundle.py OUT_DIR [--target x86_64-pc-windows-gnullvm]

Writes OUT_DIR/THIRD_PARTY_LICENSES.txt (every license file of every crate used to
build PlumSpirit.exe, plus the Rust standard library, the LLVM runtime and the
bundled SoundFont), OUT_DIR/crate_table.md (crates compiled into the executable,
including procedural macros) and OUT_DIR/build_only_table.md (build-script-only
crates) — the inventory tables used in THIRD_PARTY_NOTICES.md. Needs `cargo` on PATH (use the same wrapper as the build).

The license column is each crate's own declaration; this is an inventory, not
legal advice.
"""
import html
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def release_name():
    """The release name from the workspace version: "PlumSpirit vX.Y ~ Prototype" before
    1.0, "PlumSpirit v1.0-beta1" for a pre-release such as 1.0.0-beta.1."""
    text = open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8").read()
    m = re.search(r'^version\s*=\s*"(\d+)\.(\d+)\.\d+(?:-([a-z]+)\.(\d+))?"', text, re.M)
    if not m:
        return "PlumSpirit"
    if m.group(3):
        return f"PlumSpirit v{m.group(1)}.{m.group(2)}-{m.group(3)}{m.group(4)}"
    if (m.group(1), m.group(2)) == ("1", "0"):
        # L-S40: 1.0.0 is the owner's "v1.0-Launch" (an engineering milestone).
        return "PlumSpirit v1.0-Launch"
    return f"PlumSpirit v{m.group(1)}.{m.group(2)} ~ Prototype"
TEXTS = os.path.join(ROOT, "tools", "license-texts")
WORKSPACE = {"plumspirit", "plumspirit_engine"}
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|notice|unlicense|copyright)([-_.].*)?$", re.I)
# Published crates that ship no license file: texts taken from their repositories.
FALLBACK = {
    "accesskit": ["accesskit.txt"],
    "clipboard-win": ["clipboard-win.txt"],
    "profiling": ["profiling.txt"],
    "egui": ["egui.txt", "egui-apache.txt"],
    "eframe": ["egui.txt", "egui-apache.txt"],
    "egui-winit": ["egui.txt", "egui-apache.txt"],
    "egui_glow": ["egui.txt", "egui-apache.txt"],
    "egui-wgpu": ["egui.txt", "egui-apache.txt"],
    # v0.8: spirv (gfx-rs/rspirv, Apache-2.0) ships no license file.
    "spirv": ["rspirv-apache.txt"],
    "epaint": ["egui.txt", "egui-apache.txt"],
    "emath": ["egui.txt", "egui-apache.txt"],
    "ecolor": ["egui.txt", "egui-apache.txt"],
    "rustysynth": ["rustysynth.txt"],
    "dasp_sample": ["dasp-mit.txt", "dasp-apache.txt"],
    "gl_generator": ["gl-rs.txt"],
    # B1-0020: mlua-sys (MIT, the mlua repository) ships no license file.
    "mlua-sys": ["mlua.txt"],
    "khronos_api": ["gl-rs.txt"],
}
# License files that live below the crate root.
EXTRA_FILES = {
    "epaint_default_fonts": ["fonts/OFL.txt", "fonts/UFL.txt", "fonts/Hack-Regular.txt",
                             "fonts/emoji-icon-font-mit-license.txt"],
}
RULE = "=" * 78


def cargo(*args):
    return subprocess.run(["cargo", *args], cwd=ROOT, check=True, capture_output=True, text=True).stdout


def main():
    out_dir = sys.argv[1]
    target = "x86_64-pc-windows-gnullvm"
    if "--target" in sys.argv:
        target = sys.argv[sys.argv.index("--target") + 1]
    os.makedirs(out_dir, exist_ok=True)
    tree = cargo("tree", "-p", "plumspirit", "--target", target, "-e", "normal,build",
                 "--prefix", "none", "--no-dedupe", "-f", "{p}|{l}|{r}")
    crates = {}
    for line in tree.splitlines():
        if "|" not in line:
            continue
        p, lic, repo = line.split("|", 2)
        parts = p.split()
        name, version = parts[0], parts[1].lstrip("v")
        if name in WORKSPACE:
            continue
        crates[(name, version)] = (lic.strip() or "(not declared)", repo.strip())
    normal_tree = cargo("tree", "-p", "plumspirit", "--target", target, "-e", "normal",
                        "--prefix", "none", "--no-dedupe", "-f", "{p}")
    compiled_in = set()
    for line in normal_tree.splitlines():
        parts_ = line.split()
        if len(parts_) >= 2:
            compiled_in.add((parts_[0], parts_[1].lstrip("v")))
    meta = json.loads(cargo("metadata", "--format-version", "1", "--filter-platform", target))
    dirs = {(p["name"], p["version"]): os.path.dirname(p["manifest_path"]) for p in meta["packages"]}

    missing = []
    parts = [
        f"THIRD-PARTY LICENSE TEXTS FOR PlumSpirit.exe ({release_name()}, Windows x64)\n\n"
        "This file collects the license files shipped by each open-source crate compiled into\n"
        "PlumSpirit.exe (see THIRD_PARTY_NOTICES.md for the inventory), the Rust standard\n"
        "library, the C / unwinding runtime linked by the toolchain, and the GeneralUser GS SoundFont\n"
        "distributed next to the executable. Where a crate offers a choice of licenses, all\n"
        "of its license files are reproduced.\n"
    ]
    header = ["| Crate | Version | License (as declared) | Source |", "|---|---|---|---|"]
    table, build_only = list(header), list(header)
    for (name, version) in sorted(crates, key=lambda k: (k[0].lower(), k[1])):
        lic, repo = crates[(name, version)]
        row = f"| {name} | {version} | {lic} | {repo} |"
        (table if (name, version) in compiled_in else build_only).append(row)
        d = dirs.get((name, version))
        files = sorted(f for f in os.listdir(d) if LICENSE_FILE.match(f) and os.path.isfile(os.path.join(d, f))) if d else []
        files += [f for f in EXTRA_FILES.get(name, []) if d and os.path.isfile(os.path.join(d, f))]
        parts.append(f"\n{RULE}\n{name} {version}  —  {lic}\n{RULE}\n")
        if files:
            for f in files:
                with open(os.path.join(d, f), encoding="utf-8", errors="replace") as fh:
                    parts.append(f"--- {f} ---\n{fh.read().rstrip()}\n")
        elif name in FALLBACK:
            parts.append("(the published crate contains no license file; text taken from the project's repository)\n")
            for f in FALLBACK[name]:
                with open(os.path.join(TEXTS, f), encoding="utf-8") as fh:
                    parts.append(f"--- {f} ---\n{fh.read().rstrip()}\n")
        else:
            missing.append(f"{name} {version}")
            parts.append("(no license file found)\n")

    # Rust standard library (statically linked).
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], check=True,
                             capture_output=True, text=True).stdout.strip()
    doc = str(Path(sysroot) / "share" / "doc" / "rust")
    parts.append(f"\n{RULE}\nRust standard library (statically linked)  —  MIT OR Apache-2.0\n{RULE}\n")
    try:
        raw = open(os.path.join(doc, "COPYRIGHT-library.html"), encoding="utf-8").read()
        text = html.unescape(re.sub(r"<[^>]+>", "", raw))
        text = re.sub(r"\n\s*\n(\s*\n)+", "\n\n", text).strip()
        parts.append(f"--- COPYRIGHT-library ---\n{text}\n")
        for f in ("MIT.txt", "Apache-2.0.txt"):
            parts.append(f"--- {f} ---\n{open(os.path.join(doc, 'licenses', f), encoding='utf-8').read().rstrip()}\n")
    except OSError:
        missing.append("Rust standard library texts")
    runtime_texts = [
        ("LLVM runtime libraries (libunwind, compiler-rt builtins) statically linked by the "
         "llvm-mingw toolchain — Apache-2.0 WITH LLVM-exception", "llvm-runtime.txt"),
        # L-S41: llvm-mingw also links mingw-w64's CRT start-up code and libmingwex.
        ("mingw-w64 runtime (CRT start-up code and libmingwex) statically linked by the "
         "llvm-mingw toolchain — ZPL-2.1 and the other licenses listed", "mingw-w64-runtime.txt"),
    ] if "gnullvm" in target else [
        ("mingw-w64 runtime (CRT start-up code and libmingwex / libmsvcrt import parts) statically "
         "linked by the x86_64-pc-windows-gnu toolchain — ZPL-2.1 and the other licenses listed",
         "mingw-w64-runtime.txt"),
        ("GCC runtime (libgcc, libgcc_eh) statically linked by the x86_64-pc-windows-gnu "
         "toolchain — GPL-3.0-or-later WITH GCC-exception-3.1", "gcc-runtime-exception.txt"),
    ] if "windows-gnu" in target else []
    for title, f in runtime_texts + [
        ("GeneralUser GS v2.0.3 SoundFont by S. Christian Collins (SoundFonts/GeneralUser-GS.sf2)",
         "GeneralUser-GS-LICENSE.txt"),
        # L-S45: the mimalloc allocator (Windows builds), compiled into the exe
        # from libmimalloc-sys's C sources (its own crate licence is listed above).
    ] + ([
        ("mimalloc 2.3.2 by Microsoft Corporation, Daan Leijen (https://github.com/microsoft/mimalloc) "
         "— the memory allocator of the Windows build, compiled into PlumSpirit.exe from the "
         "libmimalloc-sys crate's C sources — MIT", "mimalloc.txt"),
    ] if "windows" in target else []) + [
        # B1-0020: the Lua interpreter, compiled into the exe from lua-src's sources.
        ("Lua 5.4.9 by Lua.org, PUC-Rio (https://www.lua.org) — the scripting interpreter, "
         "compiled into PlumSpirit.exe from the lua-src crate's sources — MIT", "lua-5.4.txt"),
        # v0.8 built-in plugins whose source licences ask for their notice.
        ("Spiderweb by Kanade Tachibana (https://github.com/UnPrioritized/Spiderweb, "
         "commit f7fd766) — ported to Rust and expanded for PlumSpirit by ouko — MIT",
         "spiderweb-MIT.txt"),
        ("SAFOR — Simple And Fast Overlaps Remover by DixelU (https://github.com/DixelU/SAFOR, "
         "commit e73cf76) — algorithm reimplemented natively in PlumSpirit — Apache-2.0",
         "safor-apache.txt"),
    ]:
        parts.append(f"\n{RULE}\n{title}\n{RULE}\n")
        parts.append(open(os.path.join(TEXTS, f), encoding="utf-8").read().rstrip() + "\n")

    with open(os.path.join(out_dir, "THIRD_PARTY_LICENSES.txt"), "w", encoding="utf-8", newline="\r\n") as fh:
        fh.write("".join(parts))
    with open(os.path.join(out_dir, "crate_table.md"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(table) + "\n")
    with open(os.path.join(out_dir, "build_only_table.md"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(build_only) + "\n")
    print(f"{len(table) - 2} compiled-in + {len(build_only) - 2} build-only crates; "
          f"missing license texts: {missing or 'none'}")


if __name__ == "__main__":
    main()
