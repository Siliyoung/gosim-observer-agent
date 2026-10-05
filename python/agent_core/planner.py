"""Decision logic: pick a pointing, fill the 16 fibres, choose exposure length and
program -- or wait / report / finish.

1. Rank visible, not-yet-done targets. Required targets that are not done yet get a
   bonus (missing one costs real points at the end). Active-request targets receive a
   temporary, deadline-aware value when they are cheap enough to pursue. Targets that
   set soon, or that have few nights left, rank higher.
2. For the best few "anchor" candidates, try each fibre as the pointing centre; fill
   every fibre with the best-value neighbour that lands on its glass; keep the best
   pointing.
3. Pick the exposure length with the best expected score per second, and the program
   (DARK / BRIGHT / BACKUP) most assigned targets will match.

Everything here uses only the public catalogue, the public scoring config, and the
agent's own past hits (SurveyState) -- never hidden weather truth. Optional nightly LLM
advice is disabled by default; when explicitly enabled, two answers are merged (see
`_night_advice`). A failed call falls back to deterministic behaviour.

This mirrors the anchor-search algorithm of this project's companion TypeScript
example target-for-target, so both examples solve the problem the same way.
"""
from __future__ import annotations

from datetime import datetime, timedelta
from typing import NamedTuple

from .geometry import (
    Moon,
    SIDEREAL_DEG_PER_SECOND,
    altaz_to_radec,
    format_utc,
    local_sidereal_deg,
    lunar_factor,
    max_hour_angle_deg,
    parse_utc,
    radec_to_altaz,
    shift_altaz,
    tangent_offsets,
    wrap180,
)
from .llm_client import LLMClient
from .memory import TraceLog
from .state import PendingPrediction

DONE_FACTOR = 0.95
PLAN_FACTOR_SAFETY = 0.9
EDGE_MARGIN_DEG = 0.08
DURATIONS = (300, 450, 600, 900, 1200, 1500, 1800, 2400, 3000, 3600)
MIN_VISIBLE_SECONDS = 600
NEIGHBOUR_RADIUS_DEG = 2.1
ANCHORS = 6
ANCHOR_POOL = 300
REQUIRED_BONUS = 60.0
CLOSED_KINDS = {"rain", "storm"}
BLOCKING_KINDS = {"terrain_obstruction", "rocket_launch"}
DIRECTION_AZ = {"N": 0.0, "NE": 45.0, "E": 90.0, "SE": 135.0, "S": 180.0,
                "SW": 225.0, "W": 270.0, "NW": 315.0}
FORECAST_AVOID_KINDS = {"rain", "storm", "overcast", "haze", "rocket_launch"}

REPORT_DROP = 0.62
REPORT_CONFIRMATIONS = 3
REPORT_SPACING_HOURS = 6.0
MAX_REPORTS = 2
# This is a planning regulariser rather than simulator overhead: without it, tiny
# score-rate differences lead to too many short exposures and poor survey coverage.
FIELD_CHANGE_COST_SECONDS = 300.0
# Do not perturb the long-horizon schedule for a numerically tiny one-step gain.
# A refinement must clear this relative-value margin over the coarse field.
REFINEMENT_MIN_RELATIVE_GAIN = 0.01


class RequestGoal(NamedTuple):
    request_id: str
    deadline: datetime
    threshold: float
    bonus: float


def _az_distance(a: float, b: float) -> float:
    return abs(wrap180(a - b))


def _bulletin_text(notices: list) -> str:
    """A human-readable rendering of a bulletin's notices, for the LLM call that reads
    "live bulletin text" rather than structured JSON."""
    if not notices:
        return "clear (no active notices)"
    return "; ".join(f"{n.get('event_kind')} {n.get('direction')}" for n in notices)


class Planner:
    def __init__(self, state, log=lambda text: None):
        self.state = state
        self.log = log
        self.grid = state.fiber_grid
        self.llm = LLMClient(log=log)
        self.trace = TraceLog(log=log)

        self.observe_count = 0
        self.reports = 0
        self.last_report_hours = float("-inf")
        self.suspicion_hours: list[float] = []
        self.night_index_seen: int | None = None
        self.consecutive_reports = 0
        self._last_forecast_notices: list = []
        self.total_assigned = 0
        self.total_hit = 0
        self.request_goals: dict[int, list[RequestGoal]] = {}
        self._request_signature: tuple = ()

        log(f"planner: {len(state.ids)} targets ({sum(state.required)} required), "
            f"{len(state.nights)} nights, llm model={self.llm.model} base_url={self.llm.base_url}")

    # -- top-level decision ----------------------------------------------------

    def decide(self, payload: dict) -> dict:
        state = self.state
        now = parse_utc(payload["now_utc"])
        hours = (now - state.survey_start).total_seconds() / 3600.0

        for message in payload.get("new_messages", []):
            if message.get("record_type") == "forecast":
                self._last_forecast_notices = message.get("notices", [])
        state.on_messages(payload.get("new_messages", []), payload.get("latest_bulletin"))
        state.on_result(payload.get("last_result"), hours)
        self._sync_active_requests(payload, now)
        last_result = payload.get("last_result")
        if last_result and last_result.get("action") == "observe":
            self.total_assigned += int(last_result.get("assigned_count", 0))
            self.total_hit += int(last_result.get("hit_count", 0))
        self._pace(payload, now)

        night = state.current_night(now)
        if night is None:
            nxt = state.next_night_start(now)
            if nxt is None:
                return {"action": "finish", "reason": "no observing night left"}
            return {"action": "wait", "until_utc": format_utc(nxt), "reason": "daytime: sleep until the next night"}
        night_index, night_start, night_end = night

        if self.night_index_seen != night_index:
            self.night_index_seen = night_index
            self._night_advice(night_start, payload)

        if (night_end - now).total_seconds() < state.min_exposure:
            nxt = state.next_night_start(now)
            if nxt is None:
                return {"action": "finish", "reason": "survey over"}
            return {"action": "wait", "until_utc": format_utc(nxt), "reason": "night ending"}

        if state.site_closed():
            return {"action": "wait", "duration_seconds": self._to_next_slot(now, night_start),
                    "reason": "bulletin: rain/storm over the whole sky"}

        report = self._maybe_report(hours, payload)
        if report is not None:
            return report

        action = self.plan(now, night_end, night_index, hours)
        if action is None:
            return {"action": "wait", "duration_seconds": self._to_next_slot(now, night_start),
                    "reason": "nothing useful is up"}
        self.observe_count += 1
        action["reason"] = f"{len(action['assignments'])} fibres, program {action['program']}"
        return action

    def on_finish(self, payload: dict) -> None:
        self.trace.write({"event": "finish", **payload})
        self.trace.close()
        self.log(f"planner: finished termination_reason={payload.get('termination_reason')} "
                 f"observes={self.observe_count} reports={self.reports} llm_calls={self.llm.calls_made}")

    def note_action(self, action: dict) -> None:
        """Called by agent.py right after an action is validated, so the consecutive-report
        counter (enforced by validation.py) stays correct even when a fallback replaced it."""
        self.consecutive_reports = self.consecutive_reports + 1 if action.get("action") == "report" else 0

    def _to_next_slot(self, now, night_start) -> int:
        slot = self.state.slot_seconds
        into = (now - night_start).total_seconds() % slot
        return int(max(60, min(3600, slot - into if into else slot)))

    def _sync_active_requests(self, payload: dict, now: datetime) -> None:
        """Turn active request progress into temporary per-target planning rewards.

        Request completion is exposure-local: a target observed before the request was
        issued must be observed again inside the request window.  Therefore these goals
        are kept separately from SurveyState.factor, and previously retired catalogue
        targets are reactivated while a request needs them.
        """
        state = self.state
        goals: dict[int, list[RequestGoal]] = {}
        signature = []
        reactivate: list[int] = []

        for request in payload.get("active_requests", []):
            completed = {str(value) for value in request.get("completed_target_ids", [])}
            minimum = max(0, int(request.get("minimum_completed", 0)))
            completed_count = int(request.get("completed_count", len(completed)))
            needed = max(0, minimum - completed_count)
            request_id = str(request.get("request_id", ""))
            if needed <= 0:
                signature.append((request_id, completed_count, 0))
                continue

            try:
                deadline = parse_utc(str(request["deadline_utc"]))
            except (KeyError, TypeError, ValueError):
                continue
            if deadline <= now:
                continue

            candidates = [str(value) for value in request.get("target_ids", [])
                          if str(value) not in completed]
            if not candidates:
                continue

            reward = max(0.0, float(request.get("completion_reward", 0.0)))
            threshold = min(1.0, max(0.0, float(request.get(
                "completion_factor_threshold", state.scoring.required_threshold))))
            hours_left = max(0.0, (deadline - now).total_seconds() / 3600.0)
            urgency = 0.5 + 0.5 * max(0.0, 1.0 - hours_left / 24.0)
            bonus = (reward / max(1, needed)) * urgency

            for target_id in candidates:
                i = state.index_of.get(target_id)
                if i is None or state.hmax[i] <= 0.0:
                    continue
                goals.setdefault(i, []).append(RequestGoal(
                    request_id=request_id,
                    deadline=deadline,
                    threshold=threshold,
                    bonus=bonus,
                ))
                reactivate.append(i)
            signature.append((request_id, completed_count, needed))

        self.request_goals = goals
        if reactivate:
            active = set(state.active)
            for i in reactivate:
                if i not in active:
                    state.active.append(i)
                    active.add(i)

        signature_tuple = tuple(sorted(signature))
        if signature_tuple != self._request_signature:
            previous_signature = self._request_signature
            self._request_signature = signature_tuple
            if signature_tuple:
                self.log(f"planner: active request progress {signature_tuple}, "
                         f"priority targets={len(goals)}")
            elif previous_signature:
                self.log("planner: no unfinished active requests")

    def _request_value(self, i: int) -> float:
        return sum(goal.bonus for goal in self.request_goals.get(i, ()))

    def _request_gain(self, i: int, reached: float, now: datetime, duration: float) -> float:
        end = now + timedelta(seconds=duration)
        return sum(goal.bonus for goal in self.request_goals.get(i, ())
                   if end <= goal.deadline and reached >= goal.threshold)

    def _required_completion_value(self, i: int, night_index: int, damp: float) -> float:
        """Value of crossing the required threshold on this target.

        Normal planning discounts repeatedly missed fields.  On the last night in
        which the target can be observed, restore the terminal penalty so a stale
        miss counter cannot silently abandon it.
        """
        state = self.state
        if not state.required[i] or state.factor[i] >= state.scoring.required_threshold:
            return 0.0
        value = max(REQUIRED_BONUS, state.scoring.required_penalty)
        return value if state.last_night[i] <= night_index else value * damp

    def _pace(self, payload: dict, now) -> None:
        """Do less work per decision when the wall clock is short for the nights still to come."""
        state = self.state
        remaining_wall = float((payload.get("wallclock") or {}).get("remaining_seconds", 1e9))
        night_seconds = sum(max(0.0, (end - max(start, now)).total_seconds()) for start, end in state.nights if end > now)
        decisions_left = max(1.0, night_seconds / 700.0)
        per_decision = remaining_wall / decisions_left
        level = 0 if per_decision > 0.12 else 1 if per_decision > 0.04 else 2
        if level != state.fast_level:
            self.log(f"planner: pace level {level} ({per_decision * 1000:.0f} ms per decision left)")
            state.fast_level = level

    # -- LLM: two calls once per night, merged -----------------------------------

    def _night_advice(self, night_start, payload: dict) -> None:
        """Two independent planning questions, asked once at the start of each night,
        each answered as {avoid_directions, duration_scale}. Their answers are merged
        (directions to avoid are unioned; the duration scale is averaged) before being
        applied to state.extra_avoid / state.duration_scale for the rest of the night."""
        state = self.state
        night_date = (night_start - timedelta(hours=12)).date().isoformat()
        left = float((payload.get("wallclock") or {}).get("remaining_seconds", 0))

        forecast_tonight = [n for n in self._last_forecast_notices if night_date in (n.get("nights") or [])]
        bulletin_notices = (payload.get("latest_bulletin") or {}).get("notices", [])
        answer_forecast = self.llm.ask_json(
            "You help schedule a telescope survey. Reply with one JSON object only: "
            '{"avoid_directions": [compass codes among N,NE,E,SE,S,SW,W,NW], "duration_scale": '
            "number 0.7-1.4}. Avoid directions with bad weather tonight, going by the forecast "
            "and the current bulletin; use a larger duration_scale when the sky looks poor.",
            {"night": night_date, "forecast_notices_for_tonight": forecast_tonight,
             "current_bulletin_notices": bulletin_notices},
            left,
        )

        learned_quality_scale = state.scale
        answer_bulletin = self.llm.ask_json(
            "You help schedule a telescope survey using tonight's live weather bulletin and the "
            'agent\'s inferred recent throughput. Reply with one JSON object only: {"avoid_directions": '
            '[compass codes among N,NE,E,SE,S,SW,W,NW], "duration_scale": number 0.7-1.4}. Avoid '
            "directions the bulletin text describes as closed or obstructed right now. Raise "
            "duration_scale when the inferred throughput scale is below 1 (poor transparency, seeing, "
            "or instrument efficiency); lower it when the scale is above 1. Fibre hit rate is supplied "
            "only as a geometry diagnostic and must not be interpreted as weather.",
            {"night": night_date, "bulletin_text": _bulletin_text(bulletin_notices),
             "inferred_throughput_scale": round(learned_quality_scale, 3),
             "fibre_hit_rate": round((self.total_hit / self.total_assigned), 3)
                 if self.total_assigned > 0 else None},
            left,
        )

        avoid = {
            str(notice.get("direction", "")).upper()
            for notice in forecast_tonight
            if notice.get("event_kind") in FORECAST_AVOID_KINDS
            and str(notice.get("direction", "")).upper() in DIRECTION_AZ
        }
        scales: list[float] = []
        for answer in (answer_forecast, answer_bulletin):
            if not answer:
                continue
            avoid |= {str(d).upper() for d in (answer.get("avoid_directions") or []) if str(d).upper() in DIRECTION_AZ}
            try:
                scales.append(min(1.4, max(0.7, float(answer.get("duration_scale", 1.0)))))
            except (TypeError, ValueError):
                pass
        state.extra_avoid = avoid
        state.duration_scale = sum(scales) / len(scales) if scales else 1.0
        source = "llm" if self.llm.enabled else "deterministic"
        self.log(f"planner: night {night_date} {source} advice (forecast call: "
                 f"{'ok' if answer_forecast else 'skipped/fallback'}, bulletin call: "
                 f"{'ok' if answer_bulletin else 'skipped/fallback'}) merged avoid={sorted(avoid)} "
                 f"duration x{state.duration_scale:.2f}")
        self.trace.write({"event": "night_advice", "night_date": night_date, "avoid": sorted(avoid),
                          "scale": state.duration_scale, "forecast_call_ok": bool(answer_forecast),
                          "bulletin_call_ok": bool(answer_bulletin)})

    # -- instrument fault reporting (deterministic rules + LLM confirmation) -----

    def _maybe_report(self, hours: float, payload: dict):
        state = self.state
        state.force_program = None
        if self.reports >= MAX_REPORTS or hours - self.last_report_hours < 24.0:
            return None
        evidence = state.fault_evidence()
        threshold = REPORT_DROP if self.reports == 0 else REPORT_DROP - 0.07
        if evidence is None or evidence.drop >= threshold:
            self.suspicion_hours = []
            return None
        if evidence.dark_checks < 6:
            state.force_program = "DARK"
        elif evidence.dark_matched < 0.5 * evidence.dark_checks:
            self.suspicion_hours = []
            return None
        if self.suspicion_hours and hours - self.suspicion_hours[-1] < REPORT_SPACING_HOURS:
            return None
        self.suspicion_hours.append(hours)
        if len(self.suspicion_hours) < REPORT_CONFIRMATIONS:
            return None
        self.suspicion_hours = []
        verdict_answer = self.llm.ask_json(
            "You check telescope data quality. A false instrument-fault report costs points, "
            'a correct one earns points. Reply with one JSON object only: {"report": true|false}.',
            evidence._asdict(), float((payload.get("wallclock") or {}).get("remaining_seconds", 0)),
        )
        verdict = verdict_answer.get("report") if isinstance(verdict_answer, dict) and \
            isinstance(verdict_answer.get("report"), bool) else None
        if verdict is False:
            self.log(f"planner: report vetoed by the model at {payload.get('now_utc')} ({evidence})")
            self.last_report_hours = hours
            return None
        self.reports += 1
        self.last_report_hours = hours
        state.forget_quality_history()
        self.log(f"planner: reporting instrument fault at {payload.get('now_utc')} evidence={evidence}")
        return {"action": "report", "reason": f"quality dropped to {evidence.drop:.0%} of the earlier level",
                "decision_source": "llm-confirmed" if verdict else "rule"}

    # -- planning value / achievability -----------------------------------------

    def _direction_factor(self, alt: float, az: float) -> float:
        state = self.state
        for direction in state.terrain:
            if direction in DIRECTION_AZ and alt < 50.0 and _az_distance(az, DIRECTION_AZ[direction]) <= 60.0:
                return 0.0
        factor = 1.0
        for key in state.notices:
            kind, _, direction = key.partition("|")
            if direction not in DIRECTION_AZ:
                continue
            near = _az_distance(az, DIRECTION_AZ[direction]) <= 67.5
            if kind in BLOCKING_KINDS and near and alt < 62.0:
                return 0.0
            if near and alt < 75.0:
                factor = min(factor, 0.35)
        for direction in state.extra_avoid:
            if direction in DIRECTION_AZ and _az_distance(az, DIRECTION_AZ[direction]) <= 67.5 and alt < 70.0:
                factor = min(factor, 0.35)
        for blocked_az, blocked_alt in state.blocked[-40:]:
            if _az_distance(az, blocked_az) <= 12.0 and alt <= blocked_alt + 3.0:
                factor = min(factor, 0.2)
        return factor

    def _value(self, i: int) -> float:
        """Planning value of fully completing target i from here (ignores how much
        exposure is achievable tonight)."""
        state = self.state
        f = state.factor[i]
        damp = 0.6 ** state.misses[i]
        threshold = state.scoring.required_threshold
        if state.required[i]:
            if f >= threshold:
                science = state.weight[i] * max(0.0, 1.0 - f * f)
            else:
                science = state.weight[i] * (1.0 - f * f) + \
                    max(REQUIRED_BONUS, state.scoring.required_penalty)
        else:
            science = 0.0 if f >= DONE_FACTOR else state.weight[i] * (1.0 - f * f)
        # Request value is exposure-local and must survive catalogue completion or
        # previous fibre misses; the current request snapshot is the source of truth.
        return science * damp + self._request_value(i)

    # -- main planning pass -------------------------------------------------------

    def plan(self, now, night_end, night_index: int, hours: float):
        state = self.state
        state.update_scale(hours)
        lst = local_sidereal_deg(now, state.lon)
        horizon = min(night_end, state.survey_end)
        seconds_left = (horizon - now).total_seconds()
        if seconds_left < state.min_exposure:
            return None
        min_visible = min(MIN_VISIBLE_SECONDS, seconds_left) * SIDEREAL_DEG_PER_SECOND

        still_active = []
        candidates: list[tuple[float, int]] = []
        for i in state.active:
            v = self._value(i)
            if v <= 0.0:
                continue
            still_active.append(i)
            ha = wrap180(lst - state.ra[i])
            h = state.hmax[i]
            if -h <= ha <= h - min_visible:
                nights_left = max(1, state.last_night[i] - night_index + 1)
                setting = (1.0 + 0.5 * max(0.0, ha / h)) if h < 180 else 1.0
                urgency = 1.0 + 2.0 / nights_left
                if (state.required[i] and state.factor[i] < state.scoring.required_threshold
                        and state.last_night[i] <= night_index):
                    # On the target's final observable night, restore the real
                    # terminal penalty to the shortlist even after earlier misses.
                    v = max(v, state.scoring.required_penalty)
                    urgency += 6.0
                candidates.append((v * urgency * setting, i))
        state.active = still_active
        if not candidates:
            return None
        candidates.sort(key=lambda t: -t[0])

        moon = Moon(now + timedelta(seconds=450), lst, state.lat)
        altaz_cache: dict[int, tuple[float, float]] = {}

        def altaz(i: int) -> tuple[float, float]:
            cached = altaz_cache.get(i)
            if cached is None:
                cached = radec_to_altaz(state.ra[i], state.dec[i], lst, state.lat)
                altaz_cache[i] = cached
            return cached

        visible = {i for _, i in candidates}
        neighbour_radius = min(NEIGHBOUR_RADIUS_DEG, 0.75 * self.grid.fov + 0.25)
        achievable_cache: dict[int, float] = {}
        scoring = state.scoring

        def achievable(i: int) -> float:
            cached = achievable_cache.get(i)
            if cached is not None:
                return cached
            alt, az = altaz(i)
            lunar = lunar_factor(moon, state.ra[i], state.dec[i], scoring.lunar_model)
            model = scoring.quality_model(alt, lunar) or 0.0
            k = (state.flux[i] * model * state.scale * PLAN_FACTOR_SAFETY) / scoring.f0t0
            ha = wrap180(lst - state.ra[i])
            up = (state.hmax[i] - ha) / SIDEREAL_DEG_PER_SECOND if state.hmax[i] < 180 else 1e9
            reach = min(1.0, k * min(state.max_exposure, up, seconds_left))
            f = state.factor[i]
            gain = state.weight[i] * max(0.0, reach * reach - f * f)
            damp = (0.6 ** state.misses[i]) * (0.7 ** state.attempts[i])
            completion_bonus = 0.0
            if state.required[i] and f < scoring.required_threshold <= reach:
                completion_bonus = self._required_completion_value(i, night_index, damp)
            request_gain = 0.0
            for goal in self.request_goals.get(i, ()):
                available = min(state.max_exposure, up, seconds_left,
                                max(0.0, (goal.deadline - now).total_seconds()))
                seconds_needed = goal.threshold / k if k > 0.0 else float("inf")
                if available >= state.min_exposure and seconds_needed <= available:
                    # Field-centre selection otherwise treats a 5-minute and a
                    # 60-minute request target as equally valuable.  Discount slow
                    # targets here; the later duration pass uses the full reward.
                    time_efficiency = min(1.0, 900.0 / max(state.min_exposure, seconds_needed))
                    request_gain += goal.bonus * time_efficiency
            result = (gain * damp + completion_bonus + request_gain) * self._direction_factor(alt, az)
            achievable_cache[i] = result
            return result

        anchors: list[tuple[float, int]] = []
        for checked, (priority, i) in enumerate(candidates):
            if checked >= ANCHOR_POOL and len(anchors) >= 3 * ANCHORS:
                break
            weighted = achievable(i) * priority / max(1e-9, self._value(i))
            if weighted > 0:
                anchors.append((weighted, i))
        if not anchors:
            return None
        anchors.sort(key=lambda t: -t[0])

        n_anchors = 1 if state.fast_level >= 1 else ANCHORS
        fibers = range(self.grid.n) if state.fast_level < 2 else self._representative_fibers()
        best = None  # (total, c_alt, c_az, chosen)
        tried = 0
        for _, anchor in anchors:
            if tried >= n_anchors and best is not None:
                break
            if tried >= n_anchors + 8:
                break
            tried += 1
            a_alt, a_az = altaz(anchor)
            near = [j for j in state.neighbours(state.ra[anchor], state.dec[anchor], neighbour_radius) if j in visible]
            near_values = {j: achievable(j) for j in near}
            for fiber in fibers:
                d_north, d_east = self.grid.fiber_center(fiber)
                c_alt, c_az = shift_altaz(a_alt, a_az, -d_north, -d_east)
                if not (state.min_alt + 1.5 <= c_alt <= 89.0):
                    continue
                c_alt = round(c_alt, 4)
                c_az = round(c_az, 4) % 360.0
                chosen: dict[int, tuple[float, int, float]] = {}  # fiber -> (score, j, margin)
                for j, v in near_values.items():
                    if v <= 0.0:
                        continue
                    alt, az = altaz(j)
                    offsets = tangent_offsets(alt, az, c_alt, c_az)
                    if offsets is None:
                        continue
                    fib, margin = self.grid.classify(*offsets)
                    if fib is None:
                        continue
                    score = v * (1.0 if margin >= EDGE_MARGIN_DEG * (1 + 1.5 * state.misses[j]) else 0.4)
                    existing = chosen.get(fib)
                    if existing is None or score > existing[0]:
                        chosen[fib] = (score, j, margin)
                if not chosen:
                    continue
                total = sum(score for score, _, _ in chosen.values())
                if best is None or total > best[0]:
                    best = (total, c_alt, c_az, chosen)
        if best is None:
            return None
        # The coarse search forces one target onto an exact fibre centre.  The true
        # optimum can lie between those centres when a small shift keeps the anchor
        # on its glass while moving several neighbours away from cell boundaries.
        # Refine only the winning field so the extra work stays bounded.
        if state.fast_level == 0:
            _seed_total, seed_alt, seed_az, _seed_chosen = best
            for north_step in (-0.25, 0.0, 0.25):
                for east_step in (-0.25, 0.0, 0.25):
                    if north_step == 0.0 and east_step == 0.0:
                        continue
                    c_alt, c_az = shift_altaz(
                        seed_alt, seed_az,
                        north_step * self.grid.pitch,
                        east_step * self.grid.pitch,
                    )
                    if not (state.min_alt + 1.5 <= c_alt <= 89.0):
                        continue
                    c_alt = round(c_alt, 4)
                    c_az = round(c_az, 4) % 360.0
                    c_ra, c_dec = altaz_to_radec(c_alt, c_az, lst, state.lat)
                    near = [j for j in state.neighbours(c_ra, c_dec, neighbour_radius) if j in visible]
                    chosen: dict[int, tuple[float, int, float]] = {}
                    for j in near:
                        v = achievable(j)
                        if v <= 0.0:
                            continue
                        alt, az = altaz(j)
                        offsets = tangent_offsets(alt, az, c_alt, c_az)
                        if offsets is None:
                            continue
                        fib, margin = self.grid.classify(*offsets)
                        if fib is None:
                            continue
                        score = v * (1.0 if margin >= EDGE_MARGIN_DEG * (1 + 1.5 * state.misses[j]) else 0.4)
                        existing = chosen.get(fib)
                        if existing is None or score > existing[0]:
                            chosen[fib] = (score, j, margin)
                    total = sum(score for score, _, _ in chosen.values())
                    if total > best[0] * (1.0 + REFINEMENT_MIN_RELATIVE_GAIN):
                        best = (total, c_alt, c_az, chosen)
        _, c_alt, c_az, chosen = best
        return self._finish_plan(now, lst, c_alt, c_az, chosen, seconds_left, moon, altaz, hours, night_index)

    def _finish_plan(self, now, lst, c_alt, c_az, chosen, seconds_left, moon, altaz, hours, night_index):
        state = self.state
        scoring = state.scoring
        c_ra, c_dec = altaz_to_radec(c_alt, c_az, lst, state.lat)
        c_hmax = max_hour_angle_deg(c_dec, state.lat, state.min_alt + 0.3)
        c_ha = wrap180(lst - c_ra)

        info: dict[int, dict] = {}
        for fiber, (_, j, _margin) in chosen.items():
            alt, az = altaz(j)
            lunar = lunar_factor(moon, state.ra[j], state.dec[j], scoring.lunar_model)
            model = scoring.quality_model(alt, lunar) or 0.0
            ha = wrap180(lst - state.ra[j])
            up = (state.hmax[j] - ha) / SIDEREAL_DEG_PER_SECOND if state.hmax[j] < 180 else 1e9
            k = (state.flux[j] * model * state.scale * PLAN_FACTOR_SAFETY) / scoring.f0t0
            info[fiber] = {"i": j, "alt": alt, "az": az, "model": model, "up": up, "k": k}
        center_up = (c_hmax - c_ha) / SIDEREAL_DEG_PER_SECOND if c_hmax < 180 else 1e9

        best = None  # (rate, duration)
        for base in DURATIONS:
            duration = round((base * state.duration_scale) / 30.0) * 30
            duration = int(max(state.min_exposure, min(state.max_exposure, duration)))
            if duration > seconds_left or duration > center_up:
                continue
            gain = 0.0
            for item in info.values():
                if item["up"] < duration:
                    continue
                reached = min(1.0, item["k"] * duration)
                f = state.factor[item["i"]]
                gain += state.weight[item["i"]] * max(0.0, reached - f)
                if state.required[item["i"]] and f < scoring.required_threshold <= reached:
                    gain += max(REQUIRED_BONUS, scoring.required_penalty)
                gain += self._request_gain(item["i"], reached, now, duration)
            rate = gain / (duration + FIELD_CHANGE_COST_SECONDS)
            if best is None or rate > best[0]:
                best = (rate, duration)
        if best is None:
            return None
        duration = best[1]
        if best[0] <= 0.0:
            if state.has_recent_sample(hours):
                return None
            fallback = next((d for d in (900, 600, 300) if d <= seconds_left and d <= center_up), None)
            if fallback is None:
                return None
            duration = fallback

        assignments: dict[str, str] = {}
        for fiber, item in info.items():
            if item["up"] >= duration:
                assignments[str(fiber)] = state.ids[item["i"]]
        if not assignments:
            return None

        band_scale = state.scale / 0.95
        votes = {"DARK": 0.0, "BRIGHT": 0.0, "BACKUP": 0.0}
        for fiber, item in info.items():
            if str(fiber) not in assignments:
                continue
            band = scoring.program_band(item["model"] * band_scale)
            votes[band] += state.weight[item["i"]] * min(1.0, item["k"] * duration) + \
                (REQUIRED_BONUS * 0.02 if state.required[item["i"]] else 0.0)
        program, best_score = "BACKUP", float("-inf")
        for name in ("DARK", "BRIGHT", "BACKUP"):
            matched = votes[name] * scoring.program_multipliers.get(name, 1.0)
            mismatched = (votes["DARK"] + votes["BRIGHT"] + votes["BACKUP"] - votes[name]) * scoring.mismatch_multiplier
            score = matched + mismatched
            if score > best_score:
                best_score, program = score, name
        if state.force_program:
            program = state.force_program

        clean = not state.all_sky_notice()
        offset_alt, offset_az = state.pointing_offset
        cmd_alt = round(c_alt - offset_alt, 4)
        cmd_az = round(c_az - offset_az, 4) % 360.0
        state.pending.clear()
        for fiber, item in info.items():
            if str(fiber) in assignments:
                state.pending[state.ids[item["i"]]] = PendingPrediction(
                    model=item["model"], band_model=item["model"] / 0.95, alt=item["alt"], az=item["az"],
                    clean=clean and self._direction_factor(item["alt"], item["az"]) >= 1.0,
                    fiber=fiber, cmd_alt=cmd_alt, cmd_az=cmd_az,
                )
        state.pending_program = program
        state.pending_duration = duration
        state.pending_night = night_index

        return {
            "action": "observe",
            "pointing": {"alt_deg": cmd_alt, "az_deg": cmd_az},
            "assignments": assignments,
            "duration_seconds": duration,
            "program": program,
        }

    def _representative_fibers(self) -> tuple[int, ...]:
        """Small, layout-independent fibre sample for emergency fast mode."""
        side = self.grid.side
        if side <= 4:
            rows = cols = sorted({side // 2, (side - 1) // 2})
        else:
            rows = cols = sorted({side // 4, side // 2, (3 * side) // 4})
        representatives = {row * side + col for row in rows for col in cols}
        return tuple(sorted(fiber for fiber in representatives if 0 <= fiber < self.grid.n))
