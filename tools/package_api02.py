#!/usr/bin/env python3
"""Create the PlumSpirit API 0.2 Windows delivery from a verified build receipt.

python -B tools/package_api02.py EXE SOUNDFONT RECEIPT NEW_OUTPUT_DIRECTORY

Set PLUMSPIRIT_PE_READER to the supported llvm-readobj executable if it is
outside PATH. The receipt records the locked build, final tests and actual EXE
run. Only verification records may change between the build and source commits.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import tomllib
import zipfile

from package_launch import ROOT, SF2_BYTES, SF2_SHA, git, pe_imports, sha, version


def file_record(path):
    return {"bytes": path.stat().st_size, "sha256": sha(path)}


def archive_files(archive, entries):
    """Write and verify each archived member against its source file."""
    with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED, compresslevel=9) as output:
        for name, source in sorted(entries.items()):
            output.write(source, name)
    with zipfile.ZipFile(archive) as output:
        if set(output.namelist()) != set(entries):
            raise SystemExit(f"Archive member mismatch in {archive.name}")
        for name, source in entries.items():
            with output.open(name) as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            if digest != sha(source) or output.getinfo(name).file_size != source.stat().st_size:
                raise SystemExit(f"Archive readback failed for {name}")


def main(exe, sf2, receipt_file, output):
    root = Path(ROOT)
    if git("status", "--porcelain"):
        raise SystemExit("Commit the final source and verification records before packaging.")
    app_version = version()
    if app_version not in ("1.0.1", "1.0.2"):
        raise SystemExit("The workspace must be version 1.0.1 or 1.0.2.")
    release = f"PlumSpirit v{app_version}"
    stem = f"PlumSpirit-v{app_version}"
    folder = f"{release} (Windows x64)"
    source_stem = f"PlumSpirit-source-v{app_version}"
    records = "docs/v1_0_1_api02/" if app_version == "1.0.1" else "docs/v1_0_2/"
    record_only = {
        records + name
        for name in (
            "CURRENT_SLICE.md", "EVIDENCE.jsonl", "FINAL_ACCEPTANCE.md",
            "RELEASE_GATE.md", "REQ_GRAPH.toml", "SESSION_CAPSULE.md",
            "TEST_IMPACT.toml",
        )
    }
    receipt = json.loads(receipt_file.read_text(encoding="utf-8"))
    head = git("rev-parse", "HEAD")
    built = git("rev-parse", "--verify", receipt["build_commit"] + "^{commit}")
    subprocess.run(["git", "-C", ROOT, "merge-base", "--is-ancestor", built, head], check=True)
    changes = git("diff", "--name-only", built, head).splitlines()
    if any(path not in record_only and not path.startswith(records + "evidence/") for path in changes):
        raise SystemExit("The source has changed since the verified executable build.")
    exe_record = file_record(exe)
    if receipt["exe"] != exe_record:
        raise SystemExit("The executable does not match the verified build receipt.")
    if (receipt["app_version"], receipt["api_version"], receipt["target"]) != (
        app_version, "0.2.0", "x86_64-pc-windows-gnullvm"
    ):
        raise SystemExit("Incorrect application, API or Windows target in the receipt.")
    if receipt["pe_version"]["file_version"] != app_version:
        raise SystemExit("Incorrect Windows file version in the receipt.")
    if receipt["native_run"]["exit_code"] != 0 or receipt["native_run"]["frames"] <= 0:
        raise SystemExit("A successful actual Windows executable run is required.")
    if receipt["native_run"]["sha256"] != exe_record["sha256"]:
        raise SystemExit("The Windows run used a different executable.")
    if receipt["app_tests"]["failed"] or receipt["app_tests"]["passed"] <= 0:
        raise SystemExit("A successful final application suite is required.")
    if receipt["engine_tests"]["failed"] or receipt["engine_tests"]["passed"] <= 0:
        raise SystemExit("Successful affected engine suites are required.")
    if sf2.stat().st_size != SF2_BYTES or sha(sf2) != SF2_SHA:
        raise SystemExit("The SoundFont must match GeneralUser GS v2.0.3.")
    imports = pe_imports(str(exe))
    if "msvcrt.dll" in imports or not any(name.startswith("api-ms-win-crt-") for name in imports):
        raise SystemExit("The executable must use the supported Windows UCRT.")
    if imports != receipt["pe_imports"]:
        raise SystemExit("The PE imports differ from the verified receipt.")

    examples = root / "crates/plumspirit/examples-lua"
    manifests = sorted(examples.glob("*/plum-plugin.toml"))
    identities = {
        tomllib.loads(path.read_text(encoding="utf-8"))["id"] for path in manifests
    }
    if len(manifests) != 22 or len(identities) != 22:
        raise SystemExit("Expected 22 distinct installable example packages.")

    output.mkdir(parents=True, exist_ok=False)
    portable = output / folder
    portable.mkdir()
    copies = {
        "PlumSpirit.exe": exe,
        "README.md": root / records / "PORTABLE_README.md",
        "docs/LUA_API.md": root / "docs/LUA_API.md",
        "docs/CHANGELOG.md": root / "CHANGELOG.md",
        "docs/FINAL_ACCEPTANCE.md": root / records / "FINAL_ACCEPTANCE.md",
        "docs/v1_0_1_api02/REQ_GRAPH.toml": root / "docs/v1_0_1_api02/REQ_GRAPH.toml",
        "docs/BUILD_RECEIPT.json": receipt_file,
        "legal/THIRD_PARTY_NOTICES.md": root / "THIRD_PARTY_NOTICES.md",
        "legal/THIRD_PARTY_LICENSES.txt": root / "THIRD_PARTY_LICENSES.txt",
        "SoundFonts/GeneralUser-GS.sf2": sf2,
        "SoundFonts/GeneralUser-GS-LICENSE.txt": root / "tools/license-texts/GeneralUser-GS-LICENSE.txt",
    }
    if app_version == "1.0.2":
        copies["docs/MUSICAL_CLIPBOARD.md"] = root / "docs/MUSICAL_CLIPBOARD.md"
    for name, source in copies.items():
        destination = portable / name
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        if file_record(destination) != file_record(source):
            raise SystemExit(f"Portable file readback failed for {name}")
    shutil.copytree(examples, portable / "examples-lua")

    files = {
        path.relative_to(output).as_posix(): path
        for path in portable.rglob("*") if path.is_file()
    }
    soundfont_member = f"{folder}/SoundFonts/GeneralUser-GS.sf2"
    portable_zip = output / f"{stem}-Windows-x64.zip"
    archive_files(portable_zip, {
        name: path for name, path in files.items()
        if app_version == "1.0.2" or name != soundfont_member
    })
    font_zip = output / f"{stem}-SoundFont.zip"
    archive_files(font_zip, {name: path for name, path in files.items() if "/SoundFonts/" in name})
    examples_zip = output / f"{stem}-Lua-Examples.zip"
    archive_files(examples_zip, {
        f"{stem}-Lua-Examples/{path.relative_to(examples).as_posix()}": path
        for path in examples.rglob("*") if path.is_file()
    })
    source_zip = output / f"{source_stem}.zip"
    subprocess.run([
        "git", "-C", ROOT, "archive", "--format=zip", "-9",
        f"--prefix={source_stem}/", "-o", str(source_zip.resolve()), head,
    ], check=True)
    with zipfile.ZipFile(source_zip) as archive:
        if archive.comment.decode("ascii") != head or archive.testzip() is not None:
            raise SystemExit("Source archive provenance or CRC verification failed.")
        for name in ("Cargo.toml", "Cargo.lock", "crates/plumspirit/src/main.rs"):
            committed = subprocess.check_output(["git", "-C", ROOT, "show", f"{head}:{name}"])
            if archive.read(f"{source_stem}/{name}") != committed:
                raise SystemExit(f"Source archive readback failed for {name}")

    manifest = {
        "release": release, "app_version": app_version, "api_version": "0.2.0",
        "source_commit": head, "exe_built_from": built,
        "verification_only_changes_after_build": changes,
        "executable": exe_record, "pe_imports": imports,
        "examples": sorted(identities), "folder": folder,
        "files": {name: file_record(path) for name, path in sorted(files.items())},
        "archives": {path.name: file_record(path) for path in (portable_zip, font_zip, examples_zip, source_zip)},
    }
    (output / "package.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"source_commit": head, "exe_built_from": built, "archives": manifest["archives"]}, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    for argument in ("exe", "sf2", "receipt_file", "output"):
        parser.add_argument(argument, type=Path)
    main(**vars(parser.parse_args()))
