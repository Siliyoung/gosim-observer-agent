//! Config snapshot taken once from `initialize.payload`: the catalogue, the
//! night calendar, per-target visibility windows, and a declination-band
//! spatial index for fast "what else is near this target" queries. Everything
//! here is derived from public config and public geometry -- never from
//! scenario truth. Learned/running state lives in `memory` instead.

use std::collections::HashMap;

use crate::protocol::{decode_targets, InitPayload, ScoringKnobs, Target};
use crate::scoring::{self, FiberGrid, LunarModel, ProgramConfig, SIDEREAL_DEG_PER_SECOND};

/// Extra margin (deg) added to `minimum_altitude_deg` before computing a
/// target's visibility window -- mirrors the reference agent's own margin,
/// so planned pointings don't sit right at the edge of the scored limit.
const ALT_MARGIN_DEG: f64 = 0.6;
/// A night only counts towards a target's `first_night`/`last_night` window
/// once it stays above the limit for at least this long.
const MIN_NIGHTLY_VISIBILITY_SECONDS: f64 = 20.0 * 60.0;

pub struct Night {
    pub start_unix: f64,
    pub end_unix: f64,
}

pub struct Config {
    pub minimum_altitude_deg: f64,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub grid: FiberGrid,
    pub min_duration_seconds: i64,
    pub max_duration_seconds: i64,
    pub q0: f64,
    pub flux_zero_point: f64,
    pub exposure_zero_point_seconds: f64,
    pub airmass_exponent: f64,
    pub lunar_model: LunarModel,
    pub program: ProgramConfig,
    pub knobs: ScoringKnobs,
    pub targets: Vec<Target>,
    pub nights: Vec<Night>,
    pub survey_start_unix: f64,
    pub survey_end_unix: f64,
    pub slot_seconds: f64,
    pub global_wallclock_seconds: f64,
    pub response_max_bytes: i64,
    index_of: HashMap<String, usize>,
    /// Declination-band index: `floor(dec_deg)` -> `(ra_deg, target index)`,
    /// sorted by `ra_deg` within each band. Mirrors the reference agent's own
    /// spatial index for `neighbours()`.
    cells: HashMap<i64, Vec<(f64, usize)>>,
}

/// The only schema versions this agent was written against. A task card
/// speaking a different one still gets a best-effort run (every field above
/// has a documented fallback) -- this is purely a heads-up in the log.
const EXPECTED_INIT_SCHEMA: &str = "v4-initialize-v1";
const EXPECTED_SCORING_SCHEMA: &str = "v4-score-v1";

impl Config {
    pub fn from_init(payload: &InitPayload) -> Config {
        if payload.schema_version != EXPECTED_INIT_SCHEMA {
            crate::memory::log(&format!(
                "state: initialize.payload.schema_version is {:?}, this agent was written against {EXPECTED_INIT_SCHEMA:?}; continuing with best-effort defaults",
                payload.schema_version
            ));
        }
        if payload.scoring.schema_version != EXPECTED_SCORING_SCHEMA {
            crate::memory::log(&format!(
                "state: scoring.schema_version is {:?}, expected {EXPECTED_SCORING_SCHEMA:?}; continuing with best-effort defaults",
                payload.scoring.schema_version
            ));
        }
        crate::memory::log(&format!(
            "state: site {:?} (sun altitude limit {:.1} deg, target altitude limit {:.1} deg)",
            payload.site.name, payload.site.sun_altitude_limit_deg, payload.site.minimum_altitude_deg
        ));

        let knobs = ScoringKnobs::from(&payload.scoring);
        let (mut targets, skipped) = decode_targets(&payload.targets);
        if skipped > 0 {
            crate::memory::log(&format!("state: skipped {skipped} target row(s) missing id/position"));
        }
        if knobs.uniformity_weight > 0.0 {
            crate::memory::log(&format!(
                "state: uniformity config present (weight {:.2}, ra band {:.1} deg) -- not separately optimized; the anchor search's \
                 own spread across visible targets is relied on instead",
                knobs.uniformity_weight, knobs.uniformity_ra_band_width_deg
            ));
        }
        crate::memory::log(&format!(
            "state: a missed required target costs {:.0} points at settlement (scoring.required.penalty_per_missing); \
             the planner's own required-target bonus is a fixed {:.0}, independent of that published figure",
            knobs.required_penalty_per_missing, crate::planner::REQUIRED_BONUS
        ));

        let nights: Vec<Night> = payload
            .survey
            .nights
            .iter()
            .filter_map(|n| {
                let start = scoring::parse_utc(&n.observing_start_utc)?;
                let end = scoring::parse_utc(&n.observing_end_utc)?;
                Some(Night { start_unix: start, end_unix: end })
            })
            .collect();
        let survey_start_unix = scoring::parse_utc(&payload.survey.start_utc).unwrap_or(0.0);
        let survey_end_unix = scoring::parse_utc(&payload.survey.end_utc).unwrap_or(f64::INFINITY);

        let instrument = &payload.instrument;
        if instrument.n_fibers != instrument.grid_side * instrument.grid_side {
            crate::memory::log(&format!(
                "state: instrument.n_fibers ({}) does not match grid_side^2 ({}); trusting grid_side for fibre indexing",
                instrument.n_fibers,
                instrument.grid_side * instrument.grid_side
            ));
        }
        let min_alt_for_window = payload.site.minimum_altitude_deg + ALT_MARGIN_DEG;
        for target in &mut targets {
            target.hmax_deg = scoring::max_hour_angle_deg(target.dec_deg, payload.site.latitude_deg, min_alt_for_window);
        }
        assign_night_windows(&mut targets, &nights, payload.site.longitude_deg);

        let index_of = targets.iter().enumerate().map(|(i, t)| (t.target_id.clone(), i)).collect();
        let cells = build_cells(&targets);

        Config {
            minimum_altitude_deg: payload.site.minimum_altitude_deg,
            latitude_deg: payload.site.latitude_deg,
            longitude_deg: payload.site.longitude_deg,
            grid: FiberGrid {
                side: instrument.grid_side,
                glass_deg: instrument.glass_side_deg,
                pitch_deg: instrument.pitch_deg,
                fov_deg: instrument.fov_side_deg,
            },
            min_duration_seconds: instrument.exposure.min_duration_seconds,
            max_duration_seconds: instrument.exposure.max_duration_seconds,
            q0: knobs.q0,
            flux_zero_point: knobs.flux_zero_point,
            exposure_zero_point_seconds: knobs.exposure_zero_point_seconds,
            airmass_exponent: knobs.airmass_exponent,
            lunar_model: knobs.lunar_model,
            program: knobs.program,
            knobs,
            targets,
            nights,
            survey_start_unix,
            survey_end_unix,
            slot_seconds: payload.survey.slot_seconds.max(1.0),
            global_wallclock_seconds: payload.limits.global_wallclock_seconds,
            response_max_bytes: payload.limits.response_max_bytes,
            index_of,
            cells,
        }
    }

    pub fn target_by_id(&self, id: &str) -> Option<&Target> {
        self.index_of.get(id).map(|&i| &self.targets[i])
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.index_of.get(id).copied()
    }

    /// `(index, start, end)` of the night containing `now_unix`, if any.
    pub fn current_night(&self, now_unix: f64) -> Option<(usize, f64, f64)> {
        self.nights
            .iter()
            .enumerate()
            .find(|(_, n)| n.start_unix <= now_unix && now_unix < n.end_unix)
            .map(|(i, n)| (i, n.start_unix, n.end_unix))
    }

    /// Start of the next night strictly after `now_unix`, if the calendar has one.
    pub fn next_night_start(&self, now_unix: f64) -> Option<f64> {
        self.nights.iter().map(|n| n.start_unix).filter(|&s| s > now_unix).min_by(|a, b| a.total_cmp(b))
    }

    /// Target indices within `radius_deg` of `(ra_deg, dec_deg)`, via the
    /// declination-band index -- avoids an O(targets) scan on every anchor trial.
    pub fn neighbours(&self, ra_deg: f64, dec_deg: f64, radius_deg: f64) -> Vec<usize> {
        let mut found = Vec::new();
        let cos_dec = (dec_deg.abs() + radius_deg).min(89.0).to_radians().cos().max(0.05);
        let width = radius_deg / cos_dec;
        let lo = (dec_deg - radius_deg).floor() as i64;
        let hi = (dec_deg + radius_deg).floor() as i64;
        for key in lo..=hi {
            let Some(band) = self.cells.get(&key) else { continue };
            let mut spans: Vec<(f64, f64)> = vec![(ra_deg - width, ra_deg + width)];
            if spans[0].0 < 0.0 {
                let overflow = spans[0].0;
                spans = vec![(0.0, spans[0].1), (overflow + 360.0, 360.0)];
            } else if spans[0].1 >= 360.0 {
                let overflow = spans[0].1;
                spans = vec![(spans[0].0, 360.0), (0.0, overflow - 360.0)];
            }
            for (low, high) in spans {
                let start = band.partition_point(|&(ra, _)| ra < low);
                let end = band.partition_point(|&(ra, _)| ra <= high);
                found.extend(band[start..end].iter().map(|&(_, i)| i));
            }
        }
        found
    }
}

fn build_cells(targets: &[Target]) -> HashMap<i64, Vec<(f64, usize)>> {
    let mut cells: HashMap<i64, Vec<(f64, usize)>> = HashMap::new();
    for (i, target) in targets.iter().enumerate() {
        if target.hmax_deg <= 0.0 {
            continue;
        }
        cells.entry(target.dec_deg.floor() as i64).or_default().push((target.ra_deg, i));
    }
    for band in cells.values_mut() {
        band.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    cells
}

/// First/last night index on which each target has at least
/// `MIN_NIGHTLY_VISIBILITY_SECONDS` above the altitude limit.
fn assign_night_windows(targets: &mut [Target], nights: &[Night], longitude_deg: f64) {
    let need_deg = MIN_NIGHTLY_VISIBILITY_SECONDS * SIDEREAL_DEG_PER_SECOND;
    let spans: Vec<(f64, f64)> = nights
        .iter()
        .map(|n| {
            let l0 = scoring::local_sidereal_deg(n.start_unix, longitude_deg);
            let span = (n.end_unix - n.start_unix) * SIDEREAL_DEG_PER_SECOND;
            (l0, span)
        })
        .collect();
    for target in targets.iter_mut() {
        if target.hmax_deg <= 0.0 {
            continue;
        }
        target.first_night = nights.len() as i64;
        target.last_night = -1;
        for (k, &(l0, span)) in spans.iter().enumerate() {
            let overlap = if target.hmax_deg >= 180.0 {
                span
            } else {
                let a = (target.ra_deg - target.hmax_deg - l0).rem_euclid(360.0);
                (span.min(a + 2.0 * target.hmax_deg) - a).max(0.0) + (span.min(a - 360.0 + 2.0 * target.hmax_deg)).max(0.0)
            };
            if overlap >= need_deg {
                if target.first_night > k as i64 {
                    target.first_night = k as i64;
                }
                target.last_night = k as i64;
            }
        }
    }
}

/// Running state of the conversation itself: how far we are and how much
/// real-wallclock budget is left. Learned sky/target knowledge lives in `memory`.
#[derive(Default)]
pub struct RunState {
    pub decisions_seen: i64,
    pub observe_actions_sent: i64,
    pub wallclock_remaining: f64,
    pub wallclock_total: f64,
    pub llm_calls_made: u32,
    pub llm_seconds_spent: f64,
}

impl RunState {
    pub fn new(wallclock_total: f64) -> Self {
        RunState {
            wallclock_remaining: wallclock_total,
            wallclock_total,
            ..Default::default()
        }
    }

    /// Conservative safety margin: once less than this remains, stop issuing
    /// LLM calls and new `observe`/`wait` actions and finish cleanly instead,
    /// rather than risk being killed mid-decision with no `finish` message.
    pub fn is_near_deadline(&self) -> bool {
        self.wallclock_remaining < 10.0
    }
}
