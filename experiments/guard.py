"""Regression and integrity guard for planner experiments."""
from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PYTHON_ROOT = ROOT / "python"
sys.path.insert(0, str(PYTHON_ROOT))


def reject_truth_access() -> bool:
    forbidden = ("local-cards/", "local-cards\\", "/truth/", "\\truth\\")
    failures: list[str] = []
    for path in (PYTHON_ROOT / "agent_core").glob("*.py"):
        text = path.read_text(encoding="utf-8").lower()
        if any(token in text for token in forbidden):
            failures.append(str(path.relative_to(ROOT)))
    if failures:
        print("forbidden local-card/truth reference: " + ", ".join(failures), file=sys.stderr)
        return False
    return True


def main() -> int:
    integrity = subprocess.run(
        [sys.executable, str(ROOT / "runner" / "verify_engine.py")],
        cwd=ROOT,
        check=False,
    ).returncode == 0
    suite = unittest.defaultTestLoader.discover(str(PYTHON_ROOT / "tests"), pattern="test_*.py")
    tests_ok = unittest.TextTestRunner(verbosity=1).run(suite).wasSuccessful()
    return 0 if integrity and tests_ok and reject_truth_access() else 1


if __name__ == "__main__":
    raise SystemExit(main())
