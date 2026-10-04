//! Decision logic: pick a pointing, fill the fibres, choose exposure length
//! and program, and sleep cleanly through the day.
//!
//! 1. Rank visible, not-yet-done targets (`value`). `required` targets not
//!    yet "safe" get a bonus (missing one costs real points at settlement).
//! 2. For the best few "anchor" candidates, try every fibre as the pointing
//!    centre (`shift_altaz`); fill every fibre with the best-value neighbour
//!    that lands on its glass; keep the best-scoring pointing.
//! 3. Pick the exposure length with the best expected gain per second, and
//!    the program (DARK/BRIGHT/BACKUP) most of the assigned targets will match.
//!
//! Everything here uses only the public catalogue, the public scoring config,
//! public bulletins/forecasts, and the agent's own past hits (`memory::Memory`)
//! -- never hidden weather truth. This closely follows the project's sibling
//! TypeScript reference agent (`examples/typescript/src/planner.ts`), which
//! was built and tuned against the same public protocol.

use std::collections::{BTreeMap, HashSet};

use crate::llm::{ask_hitrate_advice, ask_night_advice, merge_advice, LlmClient};
use crate::memory::{log, Memory, PendingPrediction};
use crate::protocol::{DecisionResponse, DecisionSnapshot, Pointing};
use crate::scoring::{self, Moon, SIDEREAL_DEG_PER_SECOND};
use crate::state::{Config, RunState};

/// Flat planning bonus for an unfinished `required` target -- not the same
/// number as the engine's own `scoring.required.penalty_per_missing` (logged
/// at startup in `state::Config::from_init`); this one only needs to outrank
/// ordinary targets in the planner's own ranking, not match the real penalty.
pub const REQUIRED_BONUS: f64 = 60.0;
const REQUIRED_SAFE_FACTOR: f64 = 0.62;
const DONE_FACTOR: f64 = 0.95;
const PLAN_FACTOR_SAFETY: f64 = 0.9;
const EDGE_MARGIN_DEG: f64 = 0.08;
const DURATIONS: [f64; 10] = [300.0, 450.0, 600.0, 900.0, 1200.0, 1500.0, 1800.0, 2400.0, 3000.0, 3600.0];
const MIN_VISIBLE_SECONDS: f64 = 600.0;
const NEIGHBOUR_RADIUS_DEG: f64 = 2.1;
const ANCHORS: usize = 3;
const ANCHOR_POOL: usize = 150;

/// A streak this long of fully-assigned, fully-missed exposures is the only
/// public signal this agent trusts enough to spend a `report` on. High on
/// purpose: a wrong `report` costs `scoring.reporting.false_penalty`, and
/// ordinary bad luck can miss a few times in a row on its own.
const FULL_MISS_REPORT_THRESHOLD: u32 = 6;

const EXPECTED_SNAPSHOT_SCHEMA: &str = "v4-decision-snapshot-v1";

pub fn decide(
    sequence: i64,
    snapshot: &DecisionSnapshot,
    config: &Config,
    run: &mut RunState,
    memory: &mut Memory,
    llm: &LlmClient,
) -> DecisionResponse {
    run.decisions_seen += 1;
    if snapshot.wallclock.remaining_seconds > 0.0 {
        run.wallclock_remaining = snapshot.wallclock.remaining_seconds;
    }
    if run.decisions_seen == 1 || run.decisions_seen % 20 == 0 {
        log_progress(snapshot, run);
    }

    memory.on_messages(config, &snapshot.new_messages, snapshot.latest_bulletin.as_ref());

    let Some(now_unix) = scoring::parse_utc(&snapshot.now_utc) else {
        log(&format!("planner: unparseable now_utc {:?}, falling back to wait", snapshot.now_utc));
        return crate::validate::safe_fallback(sequence, config, "unparseable now_utc");
    };
    let hours = (now_unix - config.survey_start_unix) / 3600.0;
    memory.on_result(config, snapshot.last_result.as_ref(), hours);

    if run.is_near_deadline() {
        return DecisionResponse::new(sequence, "finish").with_reason("wall-clock budget nearly exhausted");
    }

    // Daytime / between-nights: sleep in one hop to the next night instead of
    // polling every exposure-length `wait`.
    let Some((night_index, night_start, night_end)) = config.current_night(now_unix) else {
        return match config.next_night_start(now_unix) {
            Some(next) => {
                let mut response = DecisionResponse::new(sequence, "wait").with_reason("daytime: sleep until the next night");
                response.until_utc = Some(scoring::format_utc(next));
                validate_or_fallback(response, sequence, config)
            }
            None => DecisionResponse::new(sequence, "finish").with_reason("no observing night left"),
        };
    };

    if memory.night_seen != Some(night_index) {
        memory.night_seen = Some(night_index);
        night_llm_steps(memory, run, llm, snapshot);
    }

    if night_end - now_unix < config.min_duration_seconds as f64 {
        return match config.next_night_start(now_unix) {
            Some(next) => {
                let mut response = DecisionResponse::new(sequence, "wait").with_reason("night ending");
                response.until_utc = Some(scoring::format_utc(next));
                validate_or_fallback(response, sequence, config)
            }
            None => DecisionResponse::new(sequence, "finish").with_reason("survey over"),
        };
    }

    if memory.site_closed() {
        let mut response = DecisionResponse::new(sequence, "wait").with_reason("bulletin: rain/storm over the whole sky");
        response.duration_seconds = Some(to_next_slot(now_unix, night_start, config.slot_seconds));
        return validate_or_fallback(response, sequence, config);
    }

    if let Some(report) = maybe_report(sequence, config, memory) {
        memory.consecutive_reports += 1;
        memory.reports_issued += 1;
        return validate_or_fallback(report, sequence, config);
    }
    memory.consecutive_reports = 0;

    let response = match plan_observation(now_unix, night_end, night_index, hours, config, memory) {
        Some(plan) => {
            run.observe_actions_sent += 1;
            let reason = format!("{} fibres, program {}", plan.assignments.len(), plan.program);
            let mut response = DecisionResponse::new(sequence, "observe").with_reason(reason);
            response.pointing = Some(Pointing { alt_deg: plan.center_alt, az_deg: plan.center_az });
            response.assignments = Some(plan.assignments);
            response.duration_seconds = Some(plan.duration_seconds);
            response.program = Some(plan.program);
            response
        }
        None => {
            let mut response = DecisionResponse::new(sequence, "wait").with_reason("nothing useful is up");
            response.duration_seconds = Some(to_next_slot(now_unix, night_start, config.slot_seconds));
            response
        }
    };
    validate_or_fallback(response, sequence, config)
}

fn validate_or_fallback(response: DecisionResponse, sequence: i64, config: &Config) -> DecisionResponse {
    match crate::validate::validate(&response, config) {
        Ok(()) => response,
        Err(reason) => {
            log(&format!("planner: built an invalid {} response ({reason}); using the safe fallback", response.action));
            crate::validate::safe_fallback(sequence, config, &reason)
        }
    }
}

fn to_next_slot(now_unix: f64, night_start: f64, slot_seconds: f64) -> i64 {
    let into = (now_unix - night_start).rem_euclid(slot_seconds.max(1.0));
    ((slot_seconds - into).clamp(60.0, 3600.0)).round() as i64
}

fn log_progress(snapshot: &DecisionSnapshot, run: &RunState) {
    if snapshot.schema_version != EXPECTED_SNAPSHOT_SCHEMA {
        log(&format!(
            "planner: decision_request.payload.schema_version is {:?}, this agent was written against {EXPECTED_SNAPSHOT_SCHEMA:?}",
            snapshot.schema_version
        ));
    }
    let used_pct = if run.wallclock_total > 0.0 { 100.0 * snapshot.wallclock.elapsed_seconds / run.wallclock_total } else { 0.0 };
    log(&format!(
        "planner: decision #{} (observe #{}) now={} survey_end={} running_total={:.2} wallclock {:.1}% used",
        run.decisions_seen, snapshot.observe_action_index, snapshot.now_utc, snapshot.survey_end_utc, snapshot.running_total, used_pct
    ));
}

// ---------------------------------------------------------------------------
// LLM: two calls at the start of every observing night, merged together.
// ---------------------------------------------------------------------------

/// Runs both nightly LLM steps and merges their advice (union of
/// `avoid_directions`, average of `duration_scale`) into `memory` for the
/// rest of the night. Step A reads the public forecast recorded for tonight
/// plus the current bulletin; step B reads the same live bulletin plus the
/// agent's own hit rate so far this run. Each call retries on its own inside
/// `LlmClient`; if both end up with nothing usable (a slow endpoint, an
/// exhausted call budget, a malformed reply), tonight simply keeps the plan's
/// own default numbers (no avoided directions, duration_scale 1.0).
fn night_llm_steps(memory: &mut Memory, run: &mut RunState, llm: &LlmClient, snapshot: &DecisionSnapshot) {
    memory.extra_avoid.clear();
    memory.duration_scale = 1.0;

    let bulletin_notices = snapshot.latest_bulletin.as_ref().and_then(|b| b.get("notices")).cloned().unwrap_or(serde_json::Value::Array(vec![]));

    let forecast_context = serde_json::json!({
        "forecast_notices_recorded_for_tonight": memory.last_forecast_notices,
        "current_bulletin_notices": bulletin_notices,
    })
    .to_string();
    let advice_a = ask_night_advice(llm, run, &forecast_context);

    let hit_rate = if memory.total_assigned > 0 { memory.total_hits as f64 / memory.total_assigned as f64 } else { 0.0 };
    let hitrate_context = serde_json::json!({
        "tonight_live_bulletin_notices": bulletin_notices,
        "own_hit_rate_so_far": {"assigned": memory.total_assigned, "hits": memory.total_hits, "rate": hit_rate},
    })
    .to_string();
    let advice_b = ask_hitrate_advice(llm, run, &hitrate_context);

    match merge_advice(advice_a, advice_b) {
        Some(advice) => {
            log(&format!("llm night advice: avoid {:?} duration x{:.2}", advice.avoid_directions, advice.duration_scale));
            memory.extra_avoid = advice.avoid_directions.into_iter().collect();
            memory.duration_scale = advice.duration_scale;
        }
        None => log("llm: neither nightly call returned usable advice; tonight uses the plan's own default numbers"),
    }
}

// ---------------------------------------------------------------------------
// Conservative report heuristic
// ---------------------------------------------------------------------------

fn maybe_report(sequence: i64, config: &Config, memory: &Memory) -> Option<DecisionResponse> {
    if memory.consecutive_full_miss < FULL_MISS_REPORT_THRESHOLD {
        return None;
    }
    if memory.reports_issued >= config.knobs.false_report_free_allowance.max(1) as u32 {
        return None;
    }
    if memory.consecutive_reports + 1 >= config.knobs.max_consecutive_reports as u32 {
        return None;
    }
    log(&format!(
        "planner: {} consecutive full misses, reporting a possible instrument fault (wrong guess costs {:.0} points)",
        memory.consecutive_full_miss, config.knobs.false_penalty
    ));
    Some(DecisionResponse::new(sequence, "report").with_reason(format!(
        "{} consecutive fully-assigned exposures with zero hits",
        memory.consecutive_full_miss
    )))
}

// ---------------------------------------------------------------------------
// Planning value: how much is target `i` still worth pursuing from here
// ---------------------------------------------------------------------------

fn value(config: &Config, memory: &Memory, i: usize) -> f64 {
    let target = &config.targets[i];
    let f = memory.factor[i];
    let damp = 0.6_f64.powi(memory.misses[i] as i32);
    if target.required {
        if f >= REQUIRED_SAFE_FACTOR {
            target.science_weight * (1.0 - f * f).max(0.0) * damp
        } else {
            (target.science_weight * (1.0 - f * f) + REQUIRED_BONUS * if f < 0.5 { 1.0 } else { 0.35 }) * damp
        }
    } else if f >= DONE_FACTOR {
        0.0
    } else {
        target.science_weight * (1.0 - f * f) * damp
    }
}

/// How much completion-factor gain is realistically achievable at target `i`
/// right now, in the time actually available tonight -- the ranking used both
/// to pick anchors and to score candidate fibres within one field.
fn achievable(config: &Config, memory: &Memory, moon: &Moon, lst: f64, flux0t0: f64, seconds_left: f64, i: usize) -> f64 {
    let target = &config.targets[i];
    let (alt, az) = scoring::radec_to_altaz(target.ra_deg, target.dec_deg, lst, config.latitude_deg);
    let lunar = moon.lunar_factor(target.ra_deg, target.dec_deg);
    let model = scoring::quality_model(alt, lunar, config.q0, config.airmass_exponent);
    let k = target.feature_flux * model * memory.scale * PLAN_FACTOR_SAFETY / flux0t0;
    let up = if target.hmax_deg < 180.0 {
        (target.hmax_deg - scoring::wrap180(lst - target.ra_deg)) / SIDEREAL_DEG_PER_SECOND
    } else {
        1e9
    };
    let exposure_cap = (config.max_duration_seconds as f64).min(up).min(seconds_left);
    let reach = (k * exposure_cap).min(1.0);
    let f = memory.factor[i];
    let mut gain = target.science_weight * (reach * reach - f * f).max(0.0);
    if target.required && f < 0.5 && reach >= 0.5 {
        gain += REQUIRED_BONUS;
    }
    let damp = 0.6_f64.powi(memory.misses[i] as i32) * 0.7_f64.powi(memory.attempts[i] as i32);
    gain * damp * memory.direction_factor(alt, az)
}

// ---------------------------------------------------------------------------
// Anchor search: try several high-value targets as the pointing's anchor,
// try every fibre as the slot that anchor lands in, keep the field that
// captures the most total achievable value.
// ---------------------------------------------------------------------------

struct PlannedObserve {
    center_alt: f64,
    center_az: f64,
    assignments: BTreeMap<String, String>,
    duration_seconds: i64,
    program: String,
}

struct BestField {
    total: f64,
    center_alt: f64,
    center_az: f64,
    chosen: BTreeMap<i64, (f64, usize, f64)>, // fiber -> (score, target index, margin)
}

fn plan_observation(now_unix: f64, night_end: f64, night_index: usize, hours: f64, config: &Config, memory: &mut Memory) -> Option<PlannedObserve> {
    let lst = scoring::local_sidereal_deg(now_unix, config.longitude_deg);
    let horizon = night_end.min(config.survey_end_unix);
    let seconds_left = horizon - now_unix;
    if seconds_left < config.min_duration_seconds as f64 {
        return None;
    }
    let min_visible = MIN_VISIBLE_SECONDS.min(seconds_left) * SIDEREAL_DEG_PER_SECOND;

    let mut still_active = Vec::new();
    let mut candidates: Vec<(f64, usize)> = Vec::new();
    for &i in &memory.active {
        let v = value(config, memory, i);
        if v <= 0.0 {
            continue;
        }
        still_active.push(i);
        let target = &config.targets[i];
        let ha = scoring::wrap180(lst - target.ra_deg);
        let h = target.hmax_deg;
        if -h <= ha && ha + min_visible <= h {
            let nights_left = (target.last_night - night_index as i64 + 1).max(1) as f64;
            let setting = if h < 180.0 { 1.0 + 0.5 * (ha / h).max(0.0) } else { 1.0 };
            candidates.push((v * (1.0 + 2.0 / nights_left) * setting, i));
        }
    }
    memory.active = still_active;
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));

    let moon = Moon::at(now_unix + 450.0, config.latitude_deg, config.longitude_deg, config.lunar_model);
    let flux0t0 = (config.flux_zero_point * config.exposure_zero_point_seconds).max(1e-9);
    let visible: HashSet<usize> = candidates.iter().map(|&(_, i)| i).collect();

    let mut anchors: Vec<(f64, usize)> = Vec::new();
    for (checked, &(priority, i)) in candidates.iter().enumerate() {
        if checked >= ANCHOR_POOL && anchors.len() >= 3 * ANCHORS {
            break;
        }
        let a = achievable(config, memory, &moon, lst, flux0t0, seconds_left, i);
        let weighted = a * priority / value(config, memory, i).max(1e-9);
        if weighted > 0.0 {
            anchors.push((weighted, i));
        }
    }
    if anchors.is_empty() {
        return None;
    }
    anchors.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut best: Option<BestField> = None;
    for (tried, &(_, anchor)) in anchors.iter().enumerate() {
        if tried >= ANCHORS && best.is_some() {
            break;
        }
        if tried >= ANCHORS + 8 {
            break;
        }
        let anchor_target = &config.targets[anchor];
        let (a_alt, a_az) = scoring::radec_to_altaz(anchor_target.ra_deg, anchor_target.dec_deg, lst, config.latitude_deg);
        let near_values: BTreeMap<usize, f64> = config
            .neighbours(anchor_target.ra_deg, anchor_target.dec_deg, NEIGHBOUR_RADIUS_DEG)
            .into_iter()
            .filter(|j| visible.contains(j))
            .map(|j| (j, achievable(config, memory, &moon, lst, flux0t0, seconds_left, j)))
            .collect();

        for fiber in 0..config.grid.n_fibers() {
            let (d_north, d_east) = config.grid.fiber_center(fiber);
            let (center_alt, center_az) = scoring::shift_altaz(a_alt, a_az, -d_north, -d_east);
            if !(config.minimum_altitude_deg + 1.5 <= center_alt && center_alt <= 89.0) {
                continue;
            }
            let center_alt = (center_alt * 10000.0).round() / 10000.0;
            let center_az = (center_az * 10000.0).round() / 10000.0 % 360.0;

            let mut chosen: BTreeMap<i64, (f64, usize, f64)> = BTreeMap::new();
            for (&j, &v) in &near_values {
                if v <= 0.0 {
                    continue;
                }
                let target = &config.targets[j];
                let (alt, az) = scoring::radec_to_altaz(target.ra_deg, target.dec_deg, lst, config.latitude_deg);
                let Some((d_n, d_e)) = scoring::tangent_offsets(alt, az, center_alt, center_az) else { continue };
                let (Some(fiber), margin) = config.grid.classify(d_n, d_e) else { continue };
                let score = v * if margin >= EDGE_MARGIN_DEG * (1.0 + 1.5 * memory.misses[j] as f64) { 1.0 } else { 0.4 };
                let better = chosen.get(&fiber).map(|&(existing, _, _)| score > existing).unwrap_or(true);
                if better {
                    chosen.insert(fiber, (score, j, margin));
                }
            }
            if chosen.is_empty() {
                continue;
            }
            let total: f64 = chosen.values().map(|&(score, _, _)| score).sum();
            if best.as_ref().map(|b| total > b.total).unwrap_or(true) {
                best = Some(BestField { total, center_alt, center_az, chosen });
            }
        }
    }

    let best = best?;
    finish_plan(lst, best.center_alt, best.center_az, best.chosen, seconds_left, &moon, config, memory, hours, night_index)
}

struct FiberInfo {
    target_index: usize,
    alt: f64,
    az: f64,
    model: f64,
    up: f64,
    k: f64,
}

#[allow(clippy::too_many_arguments)]
fn finish_plan(
    lst: f64,
    center_alt: f64,
    center_az: f64,
    chosen: BTreeMap<i64, (f64, usize, f64)>,
    seconds_left: f64,
    moon: &Moon,
    config: &Config,
    memory: &mut Memory,
    hours: f64,
    night_index: usize,
) -> Option<PlannedObserve> {
    let (center_ra, center_dec) = scoring::altaz_to_radec(center_alt, center_az, lst, config.latitude_deg);
    let center_hmax = scoring::max_hour_angle_deg(center_dec, config.latitude_deg, config.minimum_altitude_deg + 0.3);
    let center_ha = scoring::wrap180(lst - center_ra);
    let flux0t0 = (config.flux_zero_point * config.exposure_zero_point_seconds).max(1e-9);

    let mut info: BTreeMap<i64, FiberInfo> = BTreeMap::new();
    for (&fiber, &(_, j, _)) in &chosen {
        let target = &config.targets[j];
        let (alt, az) = scoring::radec_to_altaz(target.ra_deg, target.dec_deg, lst, config.latitude_deg);
        let lunar = moon.lunar_factor(target.ra_deg, target.dec_deg);
        let model = scoring::quality_model(alt, lunar, config.q0, config.airmass_exponent);
        let up = if target.hmax_deg < 180.0 {
            (target.hmax_deg - scoring::wrap180(lst - target.ra_deg)) / SIDEREAL_DEG_PER_SECOND
        } else {
            1e9
        };
        let k = target.feature_flux * model * memory.scale * PLAN_FACTOR_SAFETY / flux0t0;
        info.insert(fiber, FiberInfo { target_index: j, alt, az, model, up, k });
    }
    let center_up = if center_hmax < 180.0 { (center_hmax - center_ha) / SIDEREAL_DEG_PER_SECOND } else { 1e9 };

    let mut best_duration: Option<(f64, i64)> = None;
    for &base in &DURATIONS {
        let duration = (((base * memory.duration_scale / 30.0).round()) * 30.0) as i64;
        let duration = duration.clamp(config.min_duration_seconds, config.max_duration_seconds);
        if duration as f64 > seconds_left || duration as f64 > center_up {
            continue;
        }
        let mut gain = 0.0;
        for item in info.values() {
            if item.up < duration as f64 {
                continue;
            }
            let reached = (item.k * duration as f64).min(1.0);
            let f = memory.factor[item.target_index];
            gain += config.targets[item.target_index].science_weight * (reached * reached - f * f).max(0.0);
            if config.targets[item.target_index].required && f < 0.5 && reached >= 0.5 {
                gain += REQUIRED_BONUS;
            }
        }
        let rate = gain / duration as f64;
        if best_duration.map(|(r, _)| rate > r).unwrap_or(true) {
            best_duration = Some((rate, duration));
        }
    }
    let (rate, mut duration) = best_duration?;
    if rate <= 0.0 {
        if memory.has_recent_sample(hours) {
            return None; // the estimate is fresh and says nothing improves here
        }
        // The sky estimate is stale: take one normal exposure to measure it again.
        duration = [900i64, 600, 300].into_iter().find(|&d| d as f64 <= seconds_left && d as f64 <= center_up)?;
    }

    let mut assignments: BTreeMap<String, String> = BTreeMap::new();
    for (&fiber, item) in &info {
        if item.up >= duration as f64 {
            assignments.insert(fiber.to_string(), config.targets[item.target_index].target_id.clone());
        }
    }

    let band_scale = memory.scale / 0.95;
    let mut votes: BTreeMap<&str, f64> = [("DARK", 0.0), ("BRIGHT", 0.0), ("BACKUP", 0.0)].into_iter().collect();
    for (&fiber, item) in &info {
        if !assignments.contains_key(&fiber.to_string()) {
            continue;
        }
        let band = scoring::program_band(item.model * band_scale, &config.program);
        let bonus = if config.targets[item.target_index].required { REQUIRED_BONUS * 0.02 } else { 0.0 };
        *votes.get_mut(band).unwrap() += config.targets[item.target_index].science_weight * (item.k * duration as f64).min(1.0) + bonus;
    }
    let total_votes: f64 = votes.values().sum();
    let mut program = "BACKUP";
    let mut best_score = f64::NEG_INFINITY;
    for name in ["DARK", "BRIGHT", "BACKUP"] {
        let mine = *votes.get(name).unwrap();
        let score = mine * config.program.multiplier_for(name) + (total_votes - mine) * config.program.mismatch_multiplier;
        if score > best_score {
            best_score = score;
            program = name;
        }
    }

    memory.pending.clear();
    for (&fiber, item) in &info {
        if assignments.contains_key(&fiber.to_string()) {
            memory
                .pending
                .insert(config.targets[item.target_index].target_id.clone(), PendingPrediction { model: item.model, band_model: item.model / 0.95, alt: item.alt, az: item.az });
        }
    }
    memory.pending_program = program.to_string();
    memory.pending_duration = duration as f64;
    memory.pending_night = night_index as i64;

    Some(PlannedObserve {
        center_alt,
        center_az,
        assignments,
        duration_seconds: duration,
        program: program.to_string(),
    })
}
