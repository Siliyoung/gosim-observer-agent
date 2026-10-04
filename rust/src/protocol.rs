//! `participant-agent-protocol-v4` on the wire: JSON Lines on stdin/stdout.
//! Every type here mirrors a JSON shape from the participant guide (protocol
//! section 8) -- nothing here is scenario data, it is the public envelope and
//! config schema every agent receives at `initialize` and every
//! `decision_request` after it.
//!
//! Reading is deliberately permissive (unknown keys are ignored, missing
//! optional config falls back to documented defaults) because a line this
//! agent cannot parse must never crash the process -- see `read_message`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{self, BufRead, Write};

use crate::scoring::{LunarModel, ProgramConfig};

pub const PROTOCOL_VERSION: &str = "participant-agent-protocol-v4";

// ---------------------------------------------------------------------------
// initialize.payload
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Debug)]
pub struct Site {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub latitude_deg: f64,
    #[serde(default)]
    pub longitude_deg: f64,
    #[serde(default = "default_sun_altitude_limit")]
    pub sun_altitude_limit_deg: f64,
    #[serde(default = "default_minimum_altitude")]
    pub minimum_altitude_deg: f64,
}

fn default_sun_altitude_limit() -> f64 {
    -18.0
}
fn default_minimum_altitude() -> f64 {
    30.0
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Exposure {
    #[serde(default = "default_min_duration")]
    pub min_duration_seconds: i64,
    #[serde(default = "default_max_duration")]
    pub max_duration_seconds: i64,
}

fn default_min_duration() -> i64 {
    60
}
fn default_max_duration() -> i64 {
    3600
}

#[derive(Deserialize, Clone, Debug)]
pub struct Instrument {
    #[serde(default = "default_n_fibers")]
    pub n_fibers: i64,
    #[serde(default = "default_grid_side")]
    pub grid_side: i64,
    #[serde(default)]
    pub glass_side_deg: f64,
    #[serde(default)]
    pub pitch_deg: f64,
    #[serde(default)]
    pub fov_side_deg: f64,
    #[serde(default)]
    pub exposure: Exposure,
}

fn default_n_fibers() -> i64 {
    16
}
fn default_grid_side() -> i64 {
    4
}

impl Default for Instrument {
    fn default() -> Self {
        Instrument {
            n_fibers: default_n_fibers(),
            grid_side: default_grid_side(),
            glass_side_deg: 0.0,
            pitch_deg: 0.0,
            fov_side_deg: 0.0,
            exposure: Exposure::default(),
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
struct LunarModelCfg {
    maximum_penalty: Option<f64>,
    altitude_exponent: Option<f64>,
    angular_decay_scale_deg: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct ProgramBands {
    #[serde(rename = "DARK")]
    dark: Option<f64>,
    #[serde(rename = "BRIGHT")]
    bright: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct ProgramMultipliers {
    #[serde(rename = "DARK")]
    dark: Option<f64>,
    #[serde(rename = "BRIGHT")]
    bright: Option<f64>,
    #[serde(rename = "BACKUP")]
    backup: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct ProgramCfg {
    bands: Option<ProgramBands>,
    multipliers: Option<ProgramMultipliers>,
    mismatch_multiplier: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct RequiredCfg {
    penalty_per_missing: Option<f64>,
    #[allow(dead_code)]
    observed_factor_threshold: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct UniformityCfg {
    weight: Option<f64>,
    ra_band_width_deg: Option<f64>,
    #[allow(dead_code)]
    observed_factor_threshold: Option<f64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct ReportingCfg {
    #[allow(dead_code)]
    correct_reward: Option<f64>,
    false_penalty: Option<f64>,
    false_report_free_allowance: Option<i64>,
    max_consecutive_reports: Option<i64>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct ScoringCfg {
    #[serde(default)]
    pub schema_version: String,
    q0: Option<f64>,
    flux_zero_point: Option<f64>,
    exposure_zero_point_seconds: Option<f64>,
    airmass_exponent: Option<f64>,
    lunar_model: Option<LunarModelCfg>,
    program: Option<ProgramCfg>,
    required: Option<RequiredCfg>,
    uniformity: Option<UniformityCfg>,
    reporting: Option<ReportingCfg>,
}

/// Public knobs this agent actually uses, with the guide's own worked-example
/// values as the fallback for anything a task card happens to omit.
pub struct ScoringKnobs {
    pub q0: f64,
    pub flux_zero_point: f64,
    pub exposure_zero_point_seconds: f64,
    pub airmass_exponent: f64,
    pub lunar_model: LunarModel,
    pub program: ProgramConfig,
    pub required_penalty_per_missing: f64,
    pub uniformity_weight: f64,
    pub uniformity_ra_band_width_deg: f64,
    pub false_penalty: f64,
    pub false_report_free_allowance: i64,
    pub max_consecutive_reports: i64,
}

impl From<&ScoringCfg> for ScoringKnobs {
    fn from(cfg: &ScoringCfg) -> Self {
        let defaults = ProgramConfig::default();
        let bands = cfg.program.as_ref().and_then(|p| p.bands.as_ref());
        let mults = cfg.program.as_ref().and_then(|p| p.multipliers.as_ref());
        let lunar_defaults = LunarModel::default();
        let lunar = cfg.lunar_model.as_ref();
        ScoringKnobs {
            q0: cfg.q0.unwrap_or(1.0),
            flux_zero_point: cfg.flux_zero_point.unwrap_or(0.5),
            exposure_zero_point_seconds: cfg.exposure_zero_point_seconds.unwrap_or(900.0),
            airmass_exponent: cfg.airmass_exponent.unwrap_or(0.6),
            lunar_model: LunarModel {
                maximum_penalty: lunar.and_then(|l| l.maximum_penalty).unwrap_or(lunar_defaults.maximum_penalty),
                altitude_exponent: lunar.and_then(|l| l.altitude_exponent).unwrap_or(lunar_defaults.altitude_exponent),
                angular_decay_scale_deg: lunar
                    .and_then(|l| l.angular_decay_scale_deg)
                    .unwrap_or(lunar_defaults.angular_decay_scale_deg),
            },
            program: ProgramConfig {
                band_dark: bands.and_then(|b| b.dark).unwrap_or(defaults.band_dark),
                band_bright: bands.and_then(|b| b.bright).unwrap_or(defaults.band_bright),
                multiplier_dark: mults.and_then(|m| m.dark).unwrap_or(defaults.multiplier_dark),
                multiplier_bright: mults.and_then(|m| m.bright).unwrap_or(defaults.multiplier_bright),
                multiplier_backup: mults.and_then(|m| m.backup).unwrap_or(defaults.multiplier_backup),
                mismatch_multiplier: cfg
                    .program
                    .as_ref()
                    .and_then(|p| p.mismatch_multiplier)
                    .unwrap_or(defaults.mismatch_multiplier),
            },
            required_penalty_per_missing: cfg.required.as_ref().and_then(|r| r.penalty_per_missing).unwrap_or(0.0),
            uniformity_weight: cfg.uniformity.as_ref().and_then(|u| u.weight).unwrap_or(0.0),
            uniformity_ra_band_width_deg: cfg
                .uniformity
                .as_ref()
                .and_then(|u| u.ra_band_width_deg)
                .unwrap_or(30.0),
            false_penalty: cfg.reporting.as_ref().and_then(|r| r.false_penalty).unwrap_or(0.0),
            false_report_free_allowance: cfg
                .reporting
                .as_ref()
                .and_then(|r| r.false_report_free_allowance)
                .unwrap_or(0),
            max_consecutive_reports: cfg
                .reporting
                .as_ref()
                .and_then(|r| r.max_consecutive_reports)
                .unwrap_or(32),
        }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Limits {
    #[serde(default = "default_wallclock")]
    pub global_wallclock_seconds: f64,
    #[serde(default)]
    pub response_max_bytes: i64,
}

fn default_wallclock() -> f64 {
    900.0
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct NightCfg {
    #[serde(default)]
    pub observing_start_utc: String,
    #[serde(default)]
    pub observing_end_utc: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct SurveyCfg {
    #[serde(default)]
    pub start_utc: String,
    #[serde(default)]
    pub end_utc: String,
    #[serde(default)]
    pub slot_seconds: f64,
    #[serde(default)]
    pub nights: Vec<NightCfg>,
}

#[derive(Deserialize, Clone, Debug, Default)]
pub struct TargetsTable {
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub rows: Vec<Vec<Value>>,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub target_id: String,
    pub ra_deg: f64,
    pub dec_deg: f64,
    pub feature_flux: f64,
    pub science_weight: f64,
    pub required: bool,
    /// Largest |hour angle| at which this target stays above the altitude
    /// limit (0 = never visible, 180 = always). Filled in by
    /// `state::Config::from_init` once the site's latitude is known.
    pub hmax_deg: f64,
    /// First/last night index (into `Config::nights`) on which this target
    /// gets at least 20 minutes above the limit. `last_night < first_night`
    /// means "never".
    pub first_night: i64,
    pub last_night: i64,
}

/// Decodes the column/row table into typed targets, skipping (and counting)
/// any row missing a usable `target_id` or position rather than failing the
/// whole run over one malformed row.
pub fn decode_targets(table: &TargetsTable) -> (Vec<Target>, usize) {
    let idx = |name: &str| table.columns.iter().position(|c| c == name);
    let (i_id, i_ra, i_dec) = match (idx("target_id"), idx("ra_deg"), idx("dec_deg")) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return (Vec::new(), table.rows.len()),
    };
    let i_flux = idx("feature_flux");
    let i_weight = idx("science_weight");
    let i_required = idx("required");
    let mut out = Vec::with_capacity(table.rows.len());
    let mut skipped = 0usize;
    for row in &table.rows {
        let id = row.get(i_id).and_then(|v| v.as_str());
        let ra = row.get(i_ra).and_then(|v| v.as_f64());
        let dec = row.get(i_dec).and_then(|v| v.as_f64());
        match (id, ra, dec) {
            (Some(id), Some(ra), Some(dec)) => out.push(Target {
                target_id: id.to_string(),
                ra_deg: ra,
                dec_deg: dec,
                feature_flux: i_flux.and_then(|i| row.get(i)).and_then(|v| v.as_f64()).unwrap_or(1.0),
                science_weight: i_weight.and_then(|i| row.get(i)).and_then(|v| v.as_f64()).unwrap_or(1.0),
                required: i_required
                    .and_then(|i| row.get(i))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                hmax_deg: 0.0,
                first_night: 0,
                last_night: -1,
            }),
            _ => skipped += 1,
        }
    }
    (out, skipped)
}

#[derive(Deserialize, Clone, Debug)]
pub struct InitPayload {
    #[serde(default)]
    pub schema_version: String,
    #[serde(default)]
    pub site: Site,
    #[serde(default)]
    pub survey: SurveyCfg,
    #[serde(default)]
    pub instrument: Instrument,
    #[serde(default)]
    pub scoring: ScoringCfg,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub targets: TargetsTable,
}

impl Default for Site {
    fn default() -> Self {
        Site {
            name: String::new(),
            latitude_deg: 0.0,
            longitude_deg: 0.0,
            sun_altitude_limit_deg: default_sun_altitude_limit(),
            minimum_altitude_deg: default_minimum_altitude(),
        }
    }
}

// ---------------------------------------------------------------------------
// decision_request.payload
// ---------------------------------------------------------------------------

#[derive(Deserialize, Clone, Debug, Default)]
pub struct Wallclock {
    #[serde(default)]
    pub elapsed_seconds: f64,
    #[serde(default = "default_wallclock")]
    pub remaining_seconds: f64,
}

#[derive(Deserialize, Clone, Debug)]
pub struct DecisionSnapshot {
    #[serde(default)]
    pub schema_version: String,
    pub now_utc: String,
    #[serde(default)]
    pub survey_end_utc: String,
    #[serde(default)]
    pub observe_action_index: i64,
    #[serde(default)]
    pub running_total: f64,
    #[serde(default)]
    pub wallclock: Wallclock,
    #[serde(default)]
    pub latest_bulletin: Option<Value>,
    #[serde(default)]
    pub new_messages: Vec<Value>,
    #[serde(default)]
    pub last_result: Option<Value>,
}

// ---------------------------------------------------------------------------
// decision_response (outgoing)
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, Default)]
pub struct Pointing {
    pub alt_deg: f64,
    pub az_deg: f64,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct DecisionResponse {
    pub protocol_version: &'static str,
    pub message_type: &'static str,
    pub decision_sequence: i64,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointing: Option<Pointing>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignments: Option<std::collections::BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until_utc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl DecisionResponse {
    pub fn new(decision_sequence: i64, action: &str) -> Self {
        DecisionResponse {
            protocol_version: PROTOCOL_VERSION,
            message_type: "decision_response",
            decision_sequence,
            action: action.to_string(),
            ..Default::default()
        }
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Inbound dispatch
// ---------------------------------------------------------------------------

pub enum Inbound {
    Initialize(Box<InitPayload>),
    DecisionRequest { sequence: i64, snapshot: DecisionSnapshot },
    Finish(Value),
    /// A line that parsed as JSON but did not look like a protocol message
    /// worth acting on (unknown `message_type`, or the payload did not match
    /// the expected shape). Carries a short reason for the stderr log.
    Unrecognized(String),
}

/// Reads one line from `reader`, skipping blanks, and classifies it. Returns
/// `Ok(None)` at EOF. A line that is not valid JSON is reported as
/// `Unrecognized` rather than as an `Err`, so the read loop can keep going --
/// per the guide, only *our own* malformed response ends the run, not noise
/// on the way in.
pub fn read_message<R: BufRead>(reader: &mut R) -> io::Result<Option<Inbound>> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => return Ok(Some(Inbound::Unrecognized(format!("invalid JSON ({e})")))),
        };
        let message_type = value.get("message_type").and_then(|v| v.as_str()).unwrap_or("");
        return Ok(Some(match message_type {
            "initialize" => match value.get("payload").cloned().map(serde_json::from_value::<InitPayload>) {
                Some(Ok(payload)) => Inbound::Initialize(Box::new(payload)),
                _ => Inbound::Unrecognized("initialize payload did not match the expected shape".into()),
            },
            "decision_request" => {
                let sequence = value.get("decision_sequence").and_then(|v| v.as_i64());
                let snapshot = value.get("payload").cloned().map(serde_json::from_value::<DecisionSnapshot>);
                match (sequence, snapshot) {
                    (Some(sequence), Some(Ok(snapshot))) => Inbound::DecisionRequest { sequence, snapshot },
                    _ => Inbound::Unrecognized("decision_request did not match the expected shape".into()),
                }
            }
            "finish" => Inbound::Finish(value.get("payload").cloned().unwrap_or(Value::Null)),
            other => Inbound::Unrecognized(format!("unknown message_type {other:?}")),
        }));
    }
}

/// Writes exactly one JSON line to `writer` and flushes -- the backend reads
/// line-by-line, so an unflushed response is indistinguishable from a hung agent.
pub fn write_response<W: Write>(writer: &mut W, response: &DecisionResponse) -> io::Result<()> {
    let line = serde_json::to_string(response).map_err(io::Error::other)?;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}
