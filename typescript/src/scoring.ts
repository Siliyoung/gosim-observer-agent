/**
 * Scoring helpers built only from the PUBLIC `initialize.payload.scoring` config (section 5-7 of
 * docs/participant-guide.zh.md). These mirror the engine's formulas closely enough to let the
 * planner estimate exposure quality and program bands locally; they never see hidden weather truth.
 */
import type { LunarModelConfig, ProgramConfig, ScoringConfig } from "./protocol";
import { MoonState, normalizedAirmass, separationDeg } from "./skymath";

export type ProgramName = "DARK" | "BRIGHT" | "BACKUP";

/** Public lunar quality factor L_i(t) for a target at (ra, dec), formula (14) in the guide. */
export function lunarFactor(moon: MoonState, targetRa: number, targetDec: number, model: LunarModelConfig): number {
  if (moon.alt <= 0.0) return 1.0;
  const separation = separationDeg(targetRa, targetDec, moon.ra, moon.dec);
  const penalty =
    model.maximum_penalty *
    moon.illumination *
    Math.pow(Math.sin((Math.max(0, moon.alt) * Math.PI) / 180), model.altitude_exponent) *
    Math.exp(-separation / model.angular_decay_scale_deg);
  return Math.max(0.0, Math.min(1.0, 1.0 - penalty));
}

/** `scoring.flux_zero_point * scoring.exposure_zero_point_seconds` (the f0*T0 denominator of formula 18). */
export function f0t0(scoring: ScoringConfig): number {
  return scoring.flux_zero_point * scoring.exposure_zero_point_seconds;
}

/** A rough per-second "quality model" for a target: lunar factor / (q0 * airmass^beta), used to size exposures. */
export function qualityModel(
  altDeg: number,
  lunar: number,
  scale: number,
  scoring: ScoringConfig
): number {
  const airmass = normalizedAirmass(Math.max(altDeg, 1.0));
  return (lunar * scale) / (scoring.q0 * Math.pow(airmass, scoring.airmass_exponent));
}

/** Completion factor g_i,e = min(flux * duration * quality / (f0*T0), 1), formula (18). */
export function completionFactor(flux: number, durationSeconds: number, quality: number, scoring: ScoringConfig): number {
  return Math.min((flux * durationSeconds * quality) / f0t0(scoring), 1.0);
}

/** Which program band a quality ratio (relative to scoring.program.bands) falls into. */
export function programBand(qBand: number, program: ProgramConfig): ProgramName {
  if (qBand >= program.bands.DARK) return "DARK";
  if (qBand >= program.bands.BRIGHT) return "BRIGHT";
  return "BACKUP";
}

export function programMultiplier(declared: ProgramName, actualBand: ProgramName, program: ProgramConfig): number {
  return declared === actualBand ? program.multipliers[declared] : program.mismatch_multiplier;
}
