/**
 * Action validation + deterministic fallback.
 *
 * A malformed action (bad range, unknown target, duplicate fibre, non-integer duration, ...) ends the
 * whole run as `agent_error` with no further scoring -- so every action the planner or the LLM produces
 * is checked here first. Anything that fails validation is replaced by a safe, deterministic action
 * (a bounded `wait`, or `finish` near the very end) instead of ever being sent as-is.
 */
import type { DecisionAction } from "./protocol";
import type { AgentState } from "./state";

export interface ValidationResult {
  ok: boolean;
  reason?: string;
}

/** Structural + range checks mirroring the public limits in `initialize.payload`. Never checks hidden state. */
export function validateAction(action: DecisionAction, state: AgentState, consecutiveReports: number): ValidationResult {
  if (action.action === "observe") {
    const { pointing, assignments, duration_seconds: duration, program } = action;
    if (!pointing || !Number.isFinite(pointing.alt_deg) || !Number.isFinite(pointing.az_deg)) {
      return { ok: false, reason: "pointing missing or non-finite" };
    }
    if (pointing.alt_deg < 0 || pointing.alt_deg > 90) return { ok: false, reason: "alt_deg out of [0,90]" };
    if (pointing.az_deg < 0 || pointing.az_deg >= 360) return { ok: false, reason: "az_deg out of [0,360)" };
    if (!Number.isInteger(duration) || duration < state.minExposure || duration > state.maxExposure) {
      return { ok: false, reason: `duration_seconds must be an integer in [${state.minExposure},${state.maxExposure}]` };
    }
    if (program !== undefined && !["DARK", "BRIGHT", "BACKUP"].includes(program)) {
      return { ok: false, reason: "unknown program" };
    }
    const fibersSeen = new Set<number>();
    const targetsSeen = new Set<string>();
    const nFibers = state.payload.instrument.n_fibers;
    for (const [fiberKey, targetId] of Object.entries(assignments ?? {})) {
      // "5" and "05" address the same fibre: keys must be plain non-negative integers in range.
      if (!/^\d+$/.test(fiberKey)) return { ok: false, reason: `bad fibre key ${fiberKey}` };
      const fiber = Number.parseInt(fiberKey, 10);
      if (fiber < 0 || fiber >= nFibers) return { ok: false, reason: `fibre ${fiber} out of range` };
      if (fibersSeen.has(fiber)) return { ok: false, reason: "duplicate fibre" };
      fibersSeen.add(fiber);
      if (!state.indexOf.has(targetId)) return { ok: false, reason: `unknown target ${targetId}` };
      if (targetsSeen.has(targetId)) return { ok: false, reason: "duplicate target" };
      targetsSeen.add(targetId);
    }
    return { ok: true };
  }
  if (action.action === "wait") {
    const hasDuration = "duration_seconds" in action;
    const hasUntil = "until_utc" in action;
    if (hasDuration === hasUntil) return { ok: false, reason: "wait needs exactly one of duration_seconds / until_utc" };
    if (hasDuration) {
      const d = (action as { duration_seconds: number }).duration_seconds;
      if (!Number.isInteger(d) || d < state.minExposure || d > state.maxExposure) {
        return { ok: false, reason: `wait duration_seconds must be an integer in [${state.minExposure},${state.maxExposure}]` };
      }
    } else {
      const until = (action as { until_utc: string }).until_utc;
      if (typeof until !== "string" || !until.endsWith("Z") || Number.isNaN(Date.parse(until))) {
        return { ok: false, reason: "until_utc must be a valid UTC timestamp ending in Z" };
      }
    }
    return { ok: true };
  }
  if (action.action === "report") {
    const limit = state.payload.limits.max_consecutive_reports;
    if (consecutiveReports >= limit) return { ok: false, reason: "consecutive report limit reached" };
    return { ok: true };
  }
  if (action.action === "finish") return { ok: true };
  return { ok: false, reason: "unknown action" };
}

/** A conservative action that is always valid: wait one slot, or finish if essentially no time/night is left. */
export function deterministicFallback(state: AgentState, now: Date, reason: string): DecisionAction {
  const night = state.currentNight(now);
  if (!night) {
    const next = state.nextNightStart(now);
    if (!next) return { action: "finish", reason: `${reason}: no observing time left` };
    return { action: "wait", until_utc: formatZ(next), reason: `${reason}: sleep until next night` };
  }
  const into = (now.getTime() - night.start.getTime()) / 1000 % state.slotSeconds;
  const seconds = Math.max(state.minExposure, Math.min(state.maxExposure, Math.round(state.slotSeconds - into)));
  return { action: "wait", duration_seconds: seconds, reason };
}

function formatZ(d: Date): string {
  return d.toISOString().slice(0, 19) + "Z";
}
