"""Deterministic L1-L4 research metric using the unmodified official runner.

The final stdout line is a JSON object consumed by codex-autoresearch.  This
module never opens local-card truth itself; ``evaluate`` delegates scenario
loading and settlement to the official runner.
"""
from __future__ import annotations

import json
import statistics
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(ROOT / "python"), str(ROOT / "python" / "tests")]

from evaluate_strategy import evaluate  # noqa: E402


def main() -> None:
    results = [evaluate(card) for card in ("L1", "L2", "L3", "L4")]
    for result in results:
        print(json.dumps(result, sort_keys=True), flush=True)

    scores = [float(result["total"]) for result in results]
    mean = statistics.fmean(scores)
    population_std = statistics.pstdev(scores)
    summary = {
        "metric": mean - 0.5 * population_std,
        "mean": mean,
        "worst": min(scores),
        "population_std": population_std,
        "required_missing": sum(int(result["required_missing"]) for result in results),
        "request_completed": sum(int(result["observation_requests_completed"]) for result in results),
        "scores": {result["card"]: result["total"] for result in results},
    }
    print(json.dumps(summary, sort_keys=True), flush=True)


if __name__ == "__main__":
    main()

