"""Deterministic in-process strategy benchmark for the public L1-L4 cards.

This bypasses the subprocess transport (which is POSIX-only) and disables network
LLM calls because raw runner snapshots do not carry a wallclock budget.  It exercises
the same scenario engine and scorer used by run_local.py.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT / "runner"), str(ROOT / "python")]

from challenge.v4_runner import run_scenario  # noqa: E402
from challenge.v4_workflow import V4Workflow  # noqa: E402
from agent_core.planner import Planner  # noqa: E402
from agent_core.state import SurveyState  # noqa: E402


def make_factory(init_payload: dict):
    def factory(_context):
        planner = Planner(SurveyState(init_payload))

        def agent(snapshot):
            action = planner.decide(snapshot)
            action.pop("reason", None)
            action.pop("decision_source", None)
            planner.note_action(action)
            return action

        return agent

    return factory


def evaluate(card: str) -> dict:
    root = ROOT / "local-cards" / card
    workflow = V4Workflow(root)
    init_payload = workflow.initialize_payload(900.0)
    output = ROOT / "runner" / ".strategy_eval" / card.lower()
    output.mkdir(parents=True, exist_ok=True)
    report = run_scenario(workflow.scenario_path, make_factory(init_payload), output)
    return {
        "card": card,
        "total": report["total"],
        **report["components"],
        **{key: report["counts"][key] for key in (
            "required_missing", "observation_requests_completed", "observe_actions",
            "targets_observed",
        )},
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("cards", nargs="*", default=["L1", "L2", "L3", "L4"])
    args = parser.parse_args()
    for card in args.cards:
        print(json.dumps(evaluate(card.upper()), sort_keys=True), flush=True)


if __name__ == "__main__":
    main()
