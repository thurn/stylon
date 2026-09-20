#!/usr/bin/env python3
"""Measure a release Stylon binary against the pinned Battlement corpus."""

import argparse
import hashlib
import json
import os
import pathlib
import platform
import statistics
import subprocess
import time


PINNED_REVISION = "725660cccf08e66d2151ad5c5566fc4245e7070d"
RUNS = 20
WARMUPS = 3


def output(command: list[str], cwd: pathlib.Path | None = None) -> str:
    return subprocess.check_output(command, cwd=cwd, text=True).strip()


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def measure(command: list[str]) -> list[float]:
    for _ in range(WARMUPS):
        subprocess.run(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    samples = []
    for _ in range(RUNS):
        started = time.perf_counter()
        completed = subprocess.run(
            command,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        samples.append(time.perf_counter() - started)
        if completed.returncode not in (0, 1):
            raise RuntimeError(f"Stylon exited with {completed.returncode}")
    return samples


def summary(samples: list[float]) -> dict[str, object]:
    ordered = sorted(samples)
    return {
        "median_seconds": statistics.median(samples),
        "p95_seconds": ordered[18],
        "min_seconds": ordered[0],
        "max_seconds": ordered[-1],
        "samples_seconds": samples,
    }


def sysctl(name: str) -> str | None:
    try:
        return output(["sysctl", "-n", name])
    except (FileNotFoundError, subprocess.CalledProcessError):
        return None


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--corpus", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    binary = arguments.binary.resolve()
    corpus = arguments.corpus.resolve()
    revision = output(["git", "rev-parse", "HEAD"], corpus)
    if revision != PINNED_REVISION:
        raise RuntimeError(f"expected Battlement {PINNED_REVISION}, found {revision}")

    json_command = [str(binary), "--format", "json", "--timings", str(corpus)]
    scan = subprocess.run(json_command, stdout=subprocess.PIPE, text=True, check=False)
    document = json.loads(scan.stdout)
    if scan.returncode not in (0, 1) or document["errors"]:
        raise RuntimeError("the acceptance corpus must scan without operational errors")
    human_samples = measure([str(binary), str(corpus)])
    json_samples = measure(json_command)
    rust_files = output(["git", "ls-files", "*.rs"], corpus).splitlines()
    rust_lines = sum(
        len((corpus / relative).read_bytes().splitlines()) for relative in rust_files
    )
    configuration = corpus / "stylon.toml"
    result = {
        "schema_version": 1,
        "battlement_revision": revision,
        "binary_sha256": sha256(binary),
        "configuration_sha256": sha256(configuration) if configuration.is_file() else None,
        "command": "stylon [--format json --timings] BATTLEMENT_ROOT",
        "warmups": WARMUPS,
        "runs": RUNS,
        "host": {
            "platform": platform.platform(),
            "model": sysctl("hw.model"),
            "cpu": sysctl("machdep.cpu.brand_string"),
            "physical_cores": sysctl("hw.physicalcpu"),
            "memory_bytes": sysctl("hw.memsize"),
            "power_mode": os.environ.get("STYLON_BENCHMARK_POWER_MODE", "unrecorded"),
            "rustc": output(["rustc", "--version"]),
        },
        "corpus": {
            "tracked_rust_files": len(rust_files),
            "tracked_rust_lines": rust_lines,
            "selected_files": document["summary"]["files"],
            "selected_bytes": document["summary"]["timings"]["bytes"],
            "findings": document["summary"]["findings"],
        },
        "human": summary(human_samples),
        "json": summary(json_samples),
    }
    arguments.output.write_text(json.dumps(result, indent=2) + "\n")
    if result["human"]["p95_seconds"] > 5.0 or result["json"]["p95_seconds"] > 5.0:
        raise RuntimeError("p95 exceeds the five-second contract")


if __name__ == "__main__":
    main()
