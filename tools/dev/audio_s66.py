#!/usr/bin/env python3
"""Run small S66 batches against one already-built release test binary.

Uses audio::profile_s66 and existing audio fixtures; never builds or changes
settings. Raw logs and JSONL rows are saved after each verified process.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess

ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    with open(path, "rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--binary", type=Path, required=True)
    ap.add_argument("--sf2", type=Path, required=True)
    ap.add_argument("--voices", nargs="+", type=int, choices=[256, 512, 1024, 2048, 3000, 4096], required=True)
    ap.add_argument("--cases", nargs="+", choices=["dense", "wall", "burst4096"], default=["wall"])
    ap.add_argument("--modes", nargs="+", choices=["offline", "paced"], default=["offline"])
    ap.add_argument("--repeats", type=int, choices=range(1, 6), default=3)
    ap.add_argument("--out", type=Path, default=ROOT / "docs/v0_9/audio/S66.jsonl")
    a = ap.parse_args()
    a.out = a.out.resolve()
    binary = a.binary.resolve(strict=True)
    font = a.sf2.resolve(strict=True)
    a.out.parent.mkdir(parents=True, exist_ok=True)
    logs = a.out.parent / "S66-logs"
    logs.mkdir(exist_ok=True)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    metadata = {
        "instrument_commit": commit, "binary_sha256": sha(binary),
        "soundfont_sha256": sha(font), "soundfont_bytes": font.stat().st_size,
        "os": platform.platform(), "logical_cpus": os.cpu_count(),
        "cpu_identifier": os.environ.get("PROCESSOR_IDENTIFIER", platform.processor()),
    }
    command = [str(binary), "audio::profile_s66::audio_s66_profile",
               "--ignored", "--exact", "--nocapture", "--test-threads=1"]
    for voice in a.voices:
        for case in a.cases:
            for mode in a.modes:
                for rep in range(1, a.repeats + 1):
                    run = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
                    name = f"{run}-v{voice}-{case}-{mode}-r{rep}"
                    print(f"START {name}", flush=True)
                    env = dict(os.environ, PLUMSPIRIT_TEST_SF2=str(font),
                               PLUMSPIRIT_AUDIO_VOICES=str(voice),
                               PLUMSPIRIT_AUDIO_CASE=case, PLUMSPIRIT_AUDIO_MODE=mode)
                    result = subprocess.run(command, cwd=ROOT, env=env, text=True,
                                            encoding="utf-8", capture_output=True)
                    raw = logs / f"{name}.log"
                    raw.write_text(result.stdout + result.stderr, encoding="utf-8", newline="\n")
                    if result.returncode or "test result: ok." not in result.stdout:
                        raise SystemExit(f"Measurement failed ({result.returncode}); inspect {raw}")
                    config, rows = None, []
                    for line in result.stdout.splitlines():
                        if "S66_JSON " not in line:
                            continue
                        value = json.loads(line.split("S66_JSON ", 1)[1])
                        if value.get("config"):
                            config = value
                        else:
                            if config is None:
                                raise SystemExit(f"Missing config in {raw}")
                            rows.append({**metadata, **config, **value,
                                         "run": name, "repeat": rep,
                                         "raw_log": str(raw.relative_to(ROOT)).replace("\\", "/"),
                                         "command": command})
                    expected = 2 if mode == "offline" else 4
                    if len(rows) != expected or any(r["voices"] != voice for r in rows):
                        raise SystemExit(f"Expected {expected} rows for {voice} voices in {raw}")
                    with a.out.open("a", encoding="utf-8", newline="\n") as output:
                        for row in rows:
                            output.write(json.dumps(row, ensure_ascii=False) + "\n")
                    print(f"PASS {name}: {len(rows)} rows saved", flush=True)


if __name__ == "__main__":
    main()
