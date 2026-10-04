/**
 * Decision logic: pick a pointing, fill the 16 fibres, choose exposure length and program.
 *
 * 1. Rank visible, not-yet-done targets. Required targets that are not done yet get a bonus (missing
 *    one costs 50 points at the end). Targets that set soon, or that have few nights left, rank higher.
 * 2. For the best few "anchor" candidates, try each fibre as the pointing centre; fill every fibre with
 *    the best-value neighbour that lands on its glass; keep the best pointing.
 * 3. Pick the exposure length with the best expected score per second, and the program (DARK / BRIGHT /
 *    BACKUP) most assigned targets will match.
 *
 * Everything here uses only the public catalogue, the public score config and our own past hits
 * (AgentState) -- never hidden weather truth.
 */
import type { ObserveAction } from "./protocol";
import { AgentState, PendingPrediction } from "./state";
import {
  MoonState,
  altazToRadec,
  localSiderealDeg,
  maxHourAngleDeg,
  normalizedAirmass,
  radecToAltAz,
  shiftAltaz,
  tangentOffsets,
  wrap180,
  FiberGrid,
  SIDEREAL_DEG_PER_SECOND,
} from "./skymath";
import { f0t0, lunarFactor, programBand, type ProgramName } from "./scoring";

const REQUIRED_BONUS = 60.0;
const REQUIRED_SAFE_FACTOR = 0.62;
const DONE_FACTOR = 0.95;
const PLAN_FACTOR_SAFETY = 0.9;
const EDGE_MARGIN_DEG = 0.08;
const DURATIONS = [300, 450, 600, 900, 1200, 1500, 1800, 2400, 3000, 3600];
const MIN_VISIBLE_SECONDS = 600;
const NEIGHBOUR_RADIUS_DEG = 2.1;
const ANCHORS = 3;
const ANCHOR_POOL = 150;
const CLOSED_KINDS = new Set(["rain", "storm"]);
const BLOCKING_KINDS = new Set(["terrain_obstruction", "rocket_launch"]);
const DIRECTION_AZ: Record<string, number> = { N: 0, NE: 45, E: 90, SE: 135, S: 180, SW: 225, W: 270, NW: 315 };

function azDistance(a: number, b: number): number {
  return Math.abs(wrap180(a - b));
}

export class Planner {
  readonly grid: FiberGrid;

  constructor(private readonly state: AgentState) {
    this.grid = new FiberGrid(state.payload.instrument);
  }

  private directionFactor(alt: number, az: number): number {
    let factor = 1.0;
    for (const direction of this.state.terrain) {
      if (direction in DIRECTION_AZ && alt < 50.0 && azDistance(az, DIRECTION_AZ[direction]!) <= 60.0) return 0.0;
    }
    for (const key of this.state.notices) {
      const [kind, direction] = key.split("|") as [string, string];
      if (!(direction in DIRECTION_AZ)) continue;
      const near = azDistance(az, DIRECTION_AZ[direction]!) <= 67.5;
      if (BLOCKING_KINDS.has(kind) && near && alt < 62.0) return 0.0;
      if (near && alt < 75.0) factor = Math.min(factor, 0.35);
    }
    for (const direction of this.state.extraAvoid) {
      if (direction in DIRECTION_AZ && azDistance(az, DIRECTION_AZ[direction]!) <= 67.5 && alt < 70.0) {
        factor = Math.min(factor, 0.35);
      }
    }
    const recentBlocked = this.state.blocked.slice(-40);
    for (const [blockedAz, blockedAlt] of recentBlocked) {
      if (azDistance(az, blockedAz) <= 12.0 && alt <= blockedAlt + 3.0) factor = Math.min(factor, 0.2);
    }
    return factor;
  }

  siteClosed(): boolean {
    for (const key of this.state.notices) {
      const [kind, direction] = key.split("|");
      if (kind && CLOSED_KINDS.has(kind) && direction === "ALL") return true;
    }
    return false;
  }

  /** Planning value of fully completing target i from here (ignores how much exposure is achievable tonight). */
  private value(i: number): number {
    const f = this.state.factor[i]!;
    const damp = Math.pow(0.6, this.state.misses[i]!);
    if (this.state.required[i]) {
      if (f >= REQUIRED_SAFE_FACTOR) return this.state.weight[i]! * Math.max(0.0, 1.0 - f * f) * damp;
      return (this.state.weight[i]! * (1.0 - f * f) + REQUIRED_BONUS * (f < 0.5 ? 1.0 : 0.35)) * damp;
    }
    return f >= DONE_FACTOR ? 0.0 : this.state.weight[i]! * (1.0 - f * f) * damp;
  }

  /** Return an observe action, or null when nothing useful is up right now. */
  plan(now: Date, nightEnd: Date, nightIndex: number, hours: number): ObserveAction | null {
    const st = this.state;
    st.updateScale(hours);
    const lst = localSiderealDeg(now, st.lon);
    const horizon = nightEnd < st.surveyEnd ? nightEnd : st.surveyEnd;
    const secondsLeft = (horizon.getTime() - now.getTime()) / 1000;
    if (secondsLeft < st.minExposure) return null;
    const minVisible = Math.min(MIN_VISIBLE_SECONDS, secondsLeft) * SIDEREAL_DEG_PER_SECOND;

    const stillActive: number[] = [];
    const candidates: [number, number][] = []; // (priority, index)
    for (const i of st.active) {
      const v = this.value(i);
      if (v <= 0.0) continue;
      stillActive.push(i);
      const ha = wrap180(lst - st.ra[i]!);
      const h = st.hmax[i]!;
      if (-h <= ha && ha + minVisible <= h) {
        const nightsLeft = Math.max(1, st.lastNight[i]! - nightIndex + 1);
        const setting = h < 180 ? 1.0 + 0.5 * Math.max(0.0, ha / h) : 1.0;
        candidates.push([v * (1.0 + 2.0 / nightsLeft) * setting, i]);
      }
    }
    st.active = stillActive;
    if (candidates.length === 0) return null;
    candidates.sort((a, b) => b[0] - a[0]);

    const moon = new MoonState(new Date(now.getTime() + 450_000), lst, st.lat);
    const altazCache = new Map<number, [number, number]>();
    const altaz = (i: number): [number, number] => {
      let v = altazCache.get(i);
      if (!v) {
        v = radecToAltAz(st.ra[i]!, st.dec[i]!, lst, st.lat);
        altazCache.set(i, v);
      }
      return v;
    };

    const visible = new Set(candidates.map(([, i]) => i));
    const achievableCache = new Map<number, number>();
    const scoring = st.payload.scoring;
    const flux0t0 = f0t0(scoring);
    const achievable = (i: number): number => {
      let cached = achievableCache.get(i);
      if (cached !== undefined) return cached;
      const [alt, az] = altaz(i);
      const model =
        (lunarFactor(moon, st.ra[i]!, st.dec[i]!, scoring.lunar_model) /
          (scoring.q0 * Math.pow(normalizedAirmass(Math.max(alt, 1.0)), scoring.airmass_exponent))) || 0;
      const k = (st.flux[i]! * model * st.scale * PLAN_FACTOR_SAFETY) / flux0t0;
      const up = st.hmax[i]! < 180 ? (st.hmax[i]! - wrap180(lst - st.ra[i]!)) / SIDEREAL_DEG_PER_SECOND : 1e9;
      const reach = Math.min(1.0, k * Math.min(st.maxExposure, up, secondsLeft));
      const f = st.factor[i]!;
      let gain = st.weight[i]! * Math.max(0.0, reach * reach - f * f);
      if (st.required[i] && f < 0.5 && reach >= 0.5) gain += REQUIRED_BONUS;
      const damp = Math.pow(0.6, st.misses[i]!) * Math.pow(0.7, st.attempts[i]!);
      cached = gain * damp * this.directionFactor(alt, az);
      achievableCache.set(i, cached);
      return cached;
    };

    const anchors: [number, number][] = [];
    for (let checked = 0; checked < candidates.length; checked++) {
      if (checked >= ANCHOR_POOL && anchors.length >= 3 * ANCHORS) break;
      const [priority, i] = candidates[checked]!;
      const weighted = (achievable(i) * priority) / Math.max(1e-9, this.value(i));
      if (weighted > 0) anchors.push([weighted, i]);
    }
    if (anchors.length === 0) return null;
    anchors.sort((a, b) => b[0] - a[0]);

    const nAnchors = st.fastLevel >= 1 ? 1 : ANCHORS;
    const fibers = st.fastLevel < 2 ? range(this.grid.n) : [5, 6, 9, 10];
    let best: { total: number; cAlt: number; cAz: number; chosen: Map<number, [number, number, number]> } | null = null;
    let tried = 0;
    for (const [, anchor] of anchors) {
      if (tried >= nAnchors && best !== null) break;
      if (tried >= nAnchors + 8) break;
      tried++;
      const [aAlt, aAz] = altaz(anchor);
      const near = st.neighbours(st.ra[anchor]!, st.dec[anchor]!, NEIGHBOUR_RADIUS_DEG).filter((j) => visible.has(j));
      const nearValues = new Map(near.map((j) => [j, achievable(j)]));
      for (const fiber of fibers) {
        const [dNorth, dEast] = this.grid.fiberCenter(fiber);
        let [cAlt, cAz] = shiftAltaz(aAlt, aAz, -dNorth, -dEast);
        if (!(st.minAlt + 1.5 <= cAlt && cAlt <= 89.0)) continue;
        cAlt = round4(cAlt);
        cAz = round4(cAz) % 360.0;
        const chosen = new Map<number, [number, number, number]>(); // fiber -> [score, j, margin]
        for (const [j, v] of nearValues) {
          if (v <= 0.0) continue;
          const [alt, az] = altaz(j);
          const offsets = tangentOffsets(alt, az, cAlt, cAz);
          if (offsets === null) continue;
          const [fib, margin] = this.grid.classify(offsets[0], offsets[1]);
          if (fib === null) continue;
          const score = v * (margin >= EDGE_MARGIN_DEG * (1 + 1.5 * st.misses[j]!) ? 1.0 : 0.4);
          const existing = chosen.get(fib);
          if (!existing || score > existing[0]) chosen.set(fib, [score, j, margin]);
        }
        if (chosen.size === 0) continue;
        let total = 0;
        for (const [score] of chosen.values()) total += score;
        if (best === null || total > best.total) best = { total, cAlt, cAz, chosen };
      }
    }
    if (best === null) return null;
    return this.finishPlan(now, lst, best.cAlt, best.cAz, best.chosen, secondsLeft, moon, altaz, hours, nightIndex);
  }

  private finishPlan(
    now: Date,
    lst: number,
    cAlt: number,
    cAz: number,
    chosen: Map<number, [number, number, number]>,
    secondsLeft: number,
    moon: MoonState,
    altaz: (i: number) => [number, number],
    hours: number,
    nightIndex: number
  ): ObserveAction | null {
    const st = this.state;
    const scoring = st.payload.scoring;
    const flux0t0 = f0t0(scoring);
    const [cRa, cDec] = altazToRadec(cAlt, cAz, lst, st.lat);
    const cHmax = maxHourAngleDeg(cDec, st.lat, st.minAlt + 0.3);
    const cHa = wrap180(lst - cRa);

    interface Info {
      i: number;
      alt: number;
      az: number;
      model: number;
      up: number;
      k: number;
    }
    const info = new Map<number, Info>();
    for (const [fiber, [, j]] of chosen) {
      const [alt, az] = altaz(j);
      const lunar = lunarFactor(moon, st.ra[j]!, st.dec[j]!, scoring.lunar_model);
      const model = lunar / (scoring.q0 * Math.pow(normalizedAirmass(Math.max(alt, 1.0)), scoring.airmass_exponent));
      const up = (st.hmax[j]! - wrap180(lst - st.ra[j]!)) / SIDEREAL_DEG_PER_SECOND;
      info.set(fiber, { i: j, alt, az, model, up, k: (st.flux[j]! * model * st.scale * PLAN_FACTOR_SAFETY) / flux0t0 });
    }
    const centerUp = cHmax < 180 ? (cHmax - cHa) / SIDEREAL_DEG_PER_SECOND : 1e9;

    let best: [number, number] | null = null; // [rate, duration]
    for (const base of DURATIONS) {
      let duration = Math.round((base * st.durationScale) / 30.0) * 30;
      duration = Math.max(st.minExposure, Math.min(st.maxExposure, duration));
      if (duration > secondsLeft || duration > centerUp) continue;
      let gain = 0.0;
      for (const item of info.values()) {
        if (item.up < duration) continue;
        const reached = Math.min(1.0, item.k * duration);
        const f = st.factor[item.i]!;
        gain += st.weight[item.i]! * Math.max(0.0, reached * reached - f * f);
        if (st.required[item.i] && f < 0.5 && reached >= 0.5) gain += REQUIRED_BONUS;
      }
      const rate = gain / duration;
      if (best === null || rate > best[0]) best = [rate, duration];
    }
    if (best === null) return null;
    let duration = best[1];
    if (best[0] <= 0.0) {
      if (st.hasRecentSample(hours)) return null; // the estimate is fresh and says nothing improves here
      // The sky estimate is stale: take one normal exposure to measure it again.
      const fallback = [900, 600, 300].find((d) => d <= secondsLeft && d <= centerUp);
      if (fallback === undefined) return null;
      duration = fallback;
    }

    const assignments: Record<string, string> = {};
    for (const [fiber, item] of info) {
      if (item.up >= duration) assignments[String(fiber)] = st.ids[item.i]!;
    }

    const bandScale = st.scale / 0.95;
    const votes: Record<ProgramName, number> = { DARK: 0, BRIGHT: 0, BACKUP: 0 };
    for (const [fiber, item] of info) {
      if (!(String(fiber) in assignments)) continue;
      const band = programBand(item.model * bandScale, scoring.program);
      votes[band] += st.weight[item.i]! * Math.min(1.0, item.k * duration) + (st.required[item.i] ? REQUIRED_BONUS * 0.02 : 0);
    }
    let program: ProgramName = "BACKUP";
    let bestScore = -Infinity;
    for (const name of ["DARK", "BRIGHT", "BACKUP"] as ProgramName[]) {
      const matched = votes[name] * scoring.program.multipliers[name];
      const mismatched = (votes.DARK + votes.BRIGHT + votes.BACKUP - votes[name]) * scoring.program.mismatch_multiplier;
      const score = matched + mismatched;
      if (score > bestScore) {
        bestScore = score;
        program = name;
      }
    }
    if (st.forceProgram) program = st.forceProgram;

    const clean = !st.allSkyNotice();
    st.pending.clear();
    for (const [fiber, item] of info) {
      if (String(fiber) in assignments) {
        const prediction: PendingPrediction = {
          model: item.model,
          bandModel: item.model / 0.95,
          alt: item.alt,
          az: item.az,
          clean: clean && this.directionFactor(item.alt, item.az) >= 1.0,
        };
        st.pending.set(st.ids[item.i]!, prediction);
      }
    }
    st.pendingProgram = program;
    st.pendingDuration = duration;
    st.pendingNight = nightIndex;

    return {
      action: "observe",
      pointing: { alt_deg: cAlt, az_deg: cAz },
      assignments,
      duration_seconds: duration,
      program,
    };
  }

}

function range(n: number): number[] {
  return Array.from({ length: n }, (_, i) => i);
}

function round4(x: number): number {
  return Math.round(x * 10000) / 10000;
}
