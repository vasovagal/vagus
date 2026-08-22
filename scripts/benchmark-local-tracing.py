#!/usr/bin/env python3
"""Reproducible synthetic-corpus overhead/volume gate for ADR 0029 local tracing.

The benchmark never reads the user's vault. It creates a temporary deterministic Markdown corpus,
indexes it once with an explicitly supplied local model cache, then alternates traced/untraced BM25
search and unchanged incremental-index commands. Results contain no paths or query/note content.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

MAX_TRACE_BYTES = 5 * 1024 * 1024
PRIVATE_QUERY = "synthetic-private-query-phrase latency queue retention"
ZERO_COUNTERS = (
    "rejected_operations",
    "rejected_fields",
    "rejected_types",
    "privacy_violations",
    "oversized_records",
    "writer_errors",
    "queue_drops",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=Path,
        default=Path("target/debug/vagus"),
        help="already-built all-feature vagus binary (default: target/debug/vagus)",
    )
    parser.add_argument(
        "--cache-dir",
        type=Path,
        default=Path.home() / "Library/Caches/vagus/models",
        help="complete local model cache used only for the one-time synthetic index build",
    )
    parser.add_argument("--documents", type=int, default=128)
    parser.add_argument("--pairs", type=int, default=15)
    parser.add_argument("--output", type=Path, help="write the JSON evidence artifact here")
    return parser.parse_args()


def fail(message: str) -> None:
    raise RuntimeError(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_corpus(vault: Path, documents: int) -> int:
    buckets = ("00-Inbox", "10-Projects", "20-Areas", "30-Resources", "40-Archive")
    total_bytes = 0
    for index in range(documents):
        bucket = buckets[index % len(buckets)]
        directory = vault / bucket / f"topic-{index % 16:02d}"
        directory.mkdir(parents=True, exist_ok=True)
        sections = []
        for section in range(6):
            sections.append(
                f"## Stage {section}\n\n"
                "Local offline analysis compares command latency, bounded queue pressure, rotation, "
                "retention, lexical retrieval, and privacy projection. "
                f"Synthetic document {index:04d} section {section} uses deterministic benchmark data. "
                "No production note, path, query, title, or error is copied into this corpus.\n"
            )
        text = f"# Synthetic tracing note {index:04d}\n\n" + "\n".join(sections)
        encoded = text.encode("utf-8")
        (directory / f"note-{index:04d}.md").write_bytes(encoded)
        total_bytes += len(encoded)
    return total_bytes


def base_environment(root: Path, vault: Path, cache_dir: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(root / "home"),
            "VAGUS_VAULT": str(vault),
            "VAGUS_DATA_DIR": str(root / "data"),
            "VAGUS_CACHE_DIR": str(cache_dir),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_STATE_HOME": str(root / "state"),
            "NO_COLOR": "1",
            "HF_HUB_OFFLINE": "1",
            "HF_HUB_DISABLE_TELEMETRY": "1",
        }
    )
    environment.pop("VASOVAGAL_TRACE", None)
    return environment


def invoke(
    binary: Path, environment: dict[str, str], arguments: list[str], traced: bool
) -> tuple[subprocess.CompletedProcess[bytes], float]:
    command = [str(binary)]
    if traced:
        command.append("--trace")
    command.extend(arguments)
    started = time.perf_counter_ns()
    completed = subprocess.run(command, env=environment, capture_output=True, check=False)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if completed.returncode != 0:
        fail(
            f"command failed ({completed.returncode}): {' '.join(arguments)}\n"
            f"stderr: {completed.stderr.decode('utf-8', errors='replace')}"
        )
    return completed, elapsed_ms


def measure_pairs(
    binary: Path,
    environment: dict[str, str],
    arguments: list[str],
    pairs: int,
) -> dict[str, Any]:
    # Warm process caches and both lifecycle variants before collecting paired samples.
    invoke(binary, environment, arguments, False)
    invoke(binary, environment, arguments, True)

    baseline: list[float] = []
    traced: list[float] = []
    for pair in range(pairs):
        order = (False, True) if pair % 2 == 0 else (True, False)
        outputs: dict[bool, subprocess.CompletedProcess[bytes]] = {}
        for enabled in order:
            completed, elapsed_ms = invoke(binary, environment, arguments, enabled)
            outputs[enabled] = completed
            (traced if enabled else baseline).append(elapsed_ms)
        if outputs[False].stdout != outputs[True].stdout:
            fail(f"tracing changed stdout for {' '.join(arguments)}")
        if outputs[False].stderr != outputs[True].stderr:
            fail(f"tracing changed stderr for {' '.join(arguments)}")
        if outputs[False].returncode != outputs[True].returncode:
            fail(f"tracing changed status for {' '.join(arguments)}")

    baseline_median = statistics.median(baseline)
    traced_median = statistics.median(traced)
    overhead_ms = traced_median - baseline_median
    overhead_percent = (overhead_ms / baseline_median * 100.0) if baseline_median else 0.0
    allowance_ms = max(10.0, baseline_median * 0.05)
    return {
        "pairs": pairs,
        "baseline_median_ms": round(baseline_median, 3),
        "traced_median_ms": round(traced_median, 3),
        "overhead_ms": round(overhead_ms, 3),
        "overhead_percent": round(overhead_percent, 3),
        "allowed_overhead_ms": round(allowance_ms, 3),
        "gate_passed": overhead_ms <= allowance_ms,
    }


def inspect_traces(state: Path) -> dict[str, Any]:
    files = sorted(state.rglob("trace-v1_*.jsonl"))
    if not files:
        fail("traced benchmark produced no JSONL files")
    maximum = 0
    total = 0
    summaries = 0
    private_query = PRIVATE_QUERY.encode("utf-8")
    for path in files:
        payload = path.read_bytes()
        maximum = max(maximum, len(payload))
        total += len(payload)
        if private_query in payload:
            fail("synthetic query text crossed the tracing privacy boundary")
        if not payload.endswith(b"\n"):
            fail(f"graceful trace has an unterminated tail: {path.name}")
        records = [json.loads(line) for line in payload.splitlines() if line]
        summary = [record for record in records if record.get("record_type") == "session_summary"]
        if len(summary) != 1:
            fail(f"expected one graceful summary in {path.name}, found {len(summary)}")
        summaries += 1
        counters = summary[0]["counters"]
        for counter in ZERO_COUNTERS:
            if counters[counter] != 0:
                fail(f"nonzero {counter} in {path.name}: {counters[counter]}")
    return {
        "file_count": len(files),
        "summary_count": summaries,
        "maximum_file_bytes": maximum,
        "total_bytes": total,
        "per_command_volume_gate_passed": maximum <= MAX_TRACE_BYTES,
        "zero_rejections_errors_and_drops": True,
        "query_text_absent": True,
    }


def tool_version(command: list[str]) -> str:
    try:
        return subprocess.run(command, capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return "unavailable"


def main() -> int:
    options = parse_args()
    if options.documents < 32:
        fail("--documents must be at least 32 for representative stage work")
    if options.pairs < 5:
        fail("--pairs must be at least 5")
    binary = options.binary.expanduser().resolve(strict=True)
    cache_dir = options.cache_dir.expanduser().resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        fail(f"binary is not executable: {binary}")

    with tempfile.TemporaryDirectory(prefix="vagus-tracing-benchmark-") as temporary:
        root = Path(temporary).resolve()
        for child in ("home", "config"):
            (root / child).mkdir(mode=0o700)
        vault = root / "vault"
        corpus_bytes = write_corpus(vault, options.documents)
        environment = base_environment(root, vault, cache_dir)

        initial, initial_index_ms = invoke(binary, environment, ["index"], False)
        if not initial.stdout.startswith(b"index:"):
            fail("initial index did not report its stable index summary")

        search = measure_pairs(
            binary,
            environment,
            [
                "search",
                PRIVATE_QUERY,
                "--mode",
                "bm25",
                "--no-index",
                "--json",
                "--limit",
                "15",
                "--exhaustive",
            ],
            options.pairs,
        )
        incremental_index = measure_pairs(
            binary, environment, ["index"], options.pairs
        )
        traces = inspect_traces(root / "state")

    gates_passed = (
        search["gate_passed"]
        and incremental_index["gate_passed"]
        and traces["per_command_volume_gate_passed"]
        and traces["zero_rejections_errors_and_drops"]
        and traces["query_text_absent"]
    )
    report = {
        "schema_version": 1,
        "recorded_at_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "workload": {
            "kind": "deterministic_synthetic_markdown",
            "documents": options.documents,
            "sections_per_document": 6,
            "corpus_bytes": corpus_bytes,
            "initial_index_ms": round(initial_index_ms, 3),
        },
        "environment": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "python": platform.python_version(),
            "host_rustc": tool_version(["rustc", "--version"]),
            "binary_sha256": sha256_file(binary),
        },
        "search_bm25_no_index": search,
        "incremental_index_unchanged_corpus": incremental_index,
        "traces": traces,
        "acceptance_gates_passed": gates_passed,
    }
    encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if options.output:
        output = options.output
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(encoded, encoding="utf-8")
    sys.stdout.write(encoded)
    return 0 if gates_passed else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(f"benchmark-local-tracing: {error}", file=sys.stderr)
        raise SystemExit(2)
