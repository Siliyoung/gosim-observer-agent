#!/usr/bin/env python3
"""Entry point for the python-agent example (participant-agent-protocol-v4).

Reads one JSON object per line on stdin, writes one JSON object per line on
stdout, logs only to stderr. The loop itself is deliberately thin: all the
decision-making lives in agent_core/ (protocol I/O, state tracking, geometry,
scoring, planning, the LLM client, memory/log, action validation) so this file
stays a readable map of "what happens for each message type".

LLM advice is optional and disabled by default.  Set AGENT_USE_LLM=1 to enable it;
only then is an API key required.  The deterministic scheduler handles geometry,
weather notices, requests, scoring, and fault evidence without network access.

  initialize        -> build SurveyState + Planner from the public payload
  decision_request   -> Planner.decide(), validated, sent back as decision_response
  finish              -> Planner.on_finish() logs a summary and the process exits

Any planner exception is caught here and replaced with a safe fallback action --
a bug in the strategy must never end the run as agent_error or hang the process.
"""
from __future__ import annotations

import os
import re
import sys
from pathlib import Path
from typing import Optional

if sys.version_info < (3, 9):
    sys.stderr.write("agent: Python 3.9 or newer is required\n")
    raise SystemExit(3)

from agent_core.llm_client import MissingAPIKeyError, require_api_key
from agent_core.planner import Planner
from agent_core.protocol import log, read_messages, send_response
from agent_core.state import SurveyState
from agent_core.validation import ActionRejected, fallback_action, validate_action


_ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


def load_dotenv(path: Optional[Path] = None) -> None:
    """Load simple KEY=VALUE entries from .env without overriding the environment."""
    env_path = path or Path(__file__).resolve().with_name(".env")
    try:
        lines = env_path.read_text(encoding="utf-8-sig").splitlines()
    except FileNotFoundError:
        return
    except OSError as exc:
        log(f"agent: could not read {env_path.name} ({exc})")
        return

    for line in lines:
        entry = line.strip()
        if not entry or entry.startswith("#"):
            continue
        if entry.startswith("export "):
            entry = entry[7:].lstrip()

        name, separator, value = entry.partition("=")
        name = name.strip()
        if not separator or not _ENV_NAME.fullmatch(name) or name in os.environ:
            continue

        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
            value = value[1:-1]
        os.environ[name] = value


def main() -> int:
    load_dotenv()
    try:
        require_api_key()
    except MissingAPIKeyError as exc:
        log(f"agent: {exc}")
        return 1

    state = None
    planner = None
    for message in read_messages(sys.stdin):
        kind = message.get("message_type")

        if kind == "initialize":
            try:
                state = SurveyState(message["payload"])
                planner = Planner(state, log=log)
            except Exception as exc:  # noqa: BLE001 - never crash on a malformed initialize
                log(f"agent: failed to initialize ({type(exc).__name__}: {exc}); will fall back on every decision")
                state = None
                planner = None

        elif kind == "decision_request":
            sequence = message["decision_sequence"]
            consecutive_reports = planner.consecutive_reports if planner is not None else 0
            try:
                action = planner.decide(message["payload"]) if planner is not None else fallback_action("not initialized")
                action = validate_action(action, state, consecutive_reports)
            except ActionRejected as exc:
                log(f"agent: planner produced an invalid action ({exc}); falling back")
                action = fallback_action("validation-rejected")
            except Exception as exc:  # noqa: BLE001 - a strategy bug must not end the run
                log(f"agent: planner error ({type(exc).__name__}: {exc}); falling back")
                action = fallback_action("planner-exception")
            if planner is not None:
                planner.note_action(action)
            send_response(sequence, action)

        elif kind == "finish":
            if planner is not None:
                try:
                    planner.on_finish(message.get("payload", {}))
                except Exception as exc:  # noqa: BLE001 - finish must not raise after the score is fixed
                    log(f"agent: error during finish logging ({type(exc).__name__}: {exc})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
