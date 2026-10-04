//! Validates a `decision_response` against the protocol rules in the
//! participant guide (section 8) *before* it goes out on stdout. Any
//! malformed, out-of-range or self-contradictory action ends the whole run
//! with `agent_error` on the backend's side -- there is no second chance --
//! so the planner must never get this wrong, and if it somehow does, this
//! module's `safe_fallback` is the one response we are certain is legal
//! under every published task card: a minimum-length `wait`.

use crate::protocol::DecisionResponse;
use crate::state::Config;

/// `Ok(())` when `response` is legal to send as-is. `Err(reason)` otherwise,
/// with a short human-readable reason for the stderr log.
pub fn validate(response: &DecisionResponse, config: &Config) -> Result<(), String> {
    if config.response_max_bytes > 0 {
        let encoded_len = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(usize::MAX);
        if encoded_len as i64 > config.response_max_bytes {
            return Err(format!("encoded response is {encoded_len} bytes, over the {}-byte limit", config.response_max_bytes));
        }
    }
    match response.action.as_str() {
        "observe" => validate_observe(response, config),
        "wait" => validate_wait(response, config),
        "report" | "finish" => {
            if response.pointing.is_some() || response.assignments.is_some() || response.duration_seconds.is_some() {
                return Err(format!("{} must not carry action fields", response.action));
            }
            Ok(())
        }
        other => Err(format!("unknown action {other:?}")),
    }
}

fn validate_observe(response: &DecisionResponse, config: &Config) -> Result<(), String> {
    let pointing = response.pointing.as_ref().ok_or("observe requires pointing")?;
    if !(0.0..=90.0).contains(&pointing.alt_deg) {
        return Err(format!("alt_deg {} outside [0, 90]", pointing.alt_deg));
    }
    if !(0.0..360.0).contains(&pointing.az_deg) {
        return Err(format!("az_deg {} outside [0, 360)", pointing.az_deg));
    }
    let duration = response.duration_seconds.ok_or("observe requires duration_seconds")?;
    if duration < config.min_duration_seconds || duration > config.max_duration_seconds {
        return Err(format!(
            "duration_seconds {duration} outside [{}, {}]",
            config.min_duration_seconds, config.max_duration_seconds
        ));
    }
    if let Some(program) = &response.program {
        if !matches!(program.as_str(), "DARK" | "BRIGHT" | "BACKUP") {
            return Err(format!("unknown program {program:?}"));
        }
    }
    let assignments = response.assignments.as_ref().ok_or("observe requires assignments (may be {})")?;
    let mut seen_fibers = std::collections::HashSet::new();
    let mut seen_targets = std::collections::HashSet::new();
    for (fiber_key, target_id) in assignments {
        let fiber: i64 = fiber_key
            .parse()
            .map_err(|_| format!("fiber key {fiber_key:?} is not an integer"))?;
        let n_fibers = config.grid.n_fibers();
        if !(0..n_fibers).contains(&fiber) {
            return Err(format!("fiber {fiber} outside [0, {n_fibers})"));
        }
        if !seen_fibers.insert(fiber) {
            return Err(format!("fiber {fiber} assigned twice"));
        }
        if !seen_targets.insert(target_id.clone()) {
            return Err(format!("target {target_id:?} assigned twice"));
        }
        if config.target_by_id(target_id).is_none() {
            return Err(format!("unknown target {target_id:?}"));
        }
    }
    if response.until_utc.is_some() {
        return Err("observe must not carry until_utc".into());
    }
    Ok(())
}

fn validate_wait(response: &DecisionResponse, config: &Config) -> Result<(), String> {
    if response.pointing.is_some() || response.assignments.is_some() || response.program.is_some() {
        return Err("wait must not carry observe fields".into());
    }
    match (response.duration_seconds, &response.until_utc) {
        (Some(_), Some(_)) => Err("wait must not carry both duration_seconds and until_utc".into()),
        (None, None) => Err("wait requires duration_seconds or until_utc".into()),
        (Some(duration), None) => {
            if duration < config.min_duration_seconds || duration > config.max_duration_seconds {
                Err(format!(
                    "wait duration_seconds {duration} outside [{}, {}]",
                    config.min_duration_seconds, config.max_duration_seconds
                ))
            } else {
                Ok(())
            }
        }
        (None, Some(until)) => {
            if until.ends_with('Z') {
                Ok(())
            } else {
                Err(format!("until_utc {until:?} must end with Z"))
            }
        }
    }
}

/// The one response we are certain every published task card accepts:
/// a `wait` for the shortest legal duration. Used whenever the planner's
/// intended action fails `validate`, or whenever something upstream (a
/// malformed message, an LLM-polluted field that slipped through) leaves the
/// planner unsure what else to send.
pub fn safe_fallback(decision_sequence: i64, config: &Config, reason: &str) -> DecisionResponse {
    let mut response = DecisionResponse::new(decision_sequence, "wait").with_reason(format!("validation fallback: {reason}"));
    response.duration_seconds = Some(config.min_duration_seconds);
    response
}
