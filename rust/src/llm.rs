//! LLM client: a plain OpenAI-compatible `chat/completions` call, defaulting
//! to the Kimi Coding Plan endpoint (https://www.kimi.com/code/docs/en/) --
//! `OPENAI_BASE_URL` / `OPENAI_MODEL` override the default base URL/model,
//! and the key comes from `OPENAI_API_KEY` (or `KIMI_API_KEY`). Any other
//! OpenAI-compatible endpoint works the same way by setting those three.
//!
//! A call that fails (network error, timeout, malformed reply) is retried up
//! to `MAX_ATTEMPTS` times; if every attempt fails, the night's plan uses its
//! own rule-based numbers for that one step instead, and the next scheduled
//! LLM step still runs normally. Every call also counts against `RunState`'s
//! running totals so one run never exceeds `LLM_MAX_CALLS` calls or
//! `LLM_BUDGET_SECONDS` of real time, and the planner stops attempting calls
//! once the global wall-clock budget runs low (`state::RunState`).

use serde_json::{json, Value};
use std::env;
use std::time::{Duration, Instant};

use crate::state::RunState;

const DEFAULT_BASE_URL: &str = "https://api.kimi.com/coding/v1";
const DEFAULT_MODEL: &str = "k3";
const MAX_ATTEMPTS: u32 = 3;

pub struct LlmClient {
    base_url: String,
    api_key: String,
    model: String,
    timeout: Duration,
    budget_seconds: f64,
    max_calls: u32,
}

impl LlmClient {
    /// `Err` with a plain, user-facing message when no key is configured --
    /// callers are expected to log it and exit rather than start a run that
    /// cannot plan.
    pub fn from_env() -> Result<LlmClient, String> {
        let api_key = env::var("OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| env::var("KIMI_API_KEY").ok().filter(|s| !s.trim().is_empty()))
            .map(|s| s.trim().to_string())
            .ok_or_else(|| "missing API key: set OPENAI_API_KEY".to_string())?;
        let base_url = env::var("OPENAI_BASE_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let model = env::var("OPENAI_MODEL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        let timeout_seconds: u64 = env::var("LLM_TIMEOUT_SECONDS")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .filter(|&s| s > 0)
            .unwrap_or(12);
        let budget_seconds: f64 = env::var("LLM_BUDGET_SECONDS")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .filter(|&s: &f64| s > 0.0)
            .unwrap_or(300.0);
        let max_calls: u32 = env::var("LLM_MAX_CALLS")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(100);
        Ok(LlmClient {
            base_url,
            api_key,
            model,
            timeout: Duration::from_secs(timeout_seconds),
            budget_seconds,
            max_calls,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    fn attempt_chat(&self, system: &str, user: &str) -> Result<String, String> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let body = json!({
            "model": self.model,
            "temperature": 0.2,
            "max_tokens": 220,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });
        let response = ureq::post(&url)
            .timeout(self.timeout)
            .set("Authorization", &format!("Bearer {}", self.api_key))
            .set("Content-Type", "application/json")
            .send_json(body)
            .map_err(|e| format!("request failed ({e})"))?;
        let parsed: Value = response.into_json().map_err(|e| format!("response was not JSON ({e})"))?;
        parsed
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| "response had no choices[0].message.content".to_string())
    }

    /// Retries up to `MAX_ATTEMPTS` times, charging every attempt against
    /// `run`'s call count and time budget. Stops early (without spending an
    /// attempt) once the run is close to its global wall-clock deadline, or
    /// once this run's own LLM call/time budget is used up.
    fn chat(&self, system: &str, user: &str, run: &mut RunState) -> Option<String> {
        for attempt in 1..=MAX_ATTEMPTS {
            if run.llm_calls_made >= self.max_calls || run.llm_seconds_spent >= self.budget_seconds || run.wallclock_remaining <= 30.0 {
                return None;
            }
            run.llm_calls_made += 1;
            let started = Instant::now();
            let result = self.attempt_chat(system, user);
            run.llm_seconds_spent += started.elapsed().as_secs_f64();
            match result {
                Ok(content) => return Some(content),
                Err(reason) => crate::memory::log(&format!("llm: attempt {attempt}/{MAX_ATTEMPTS} failed ({reason})")),
            }
        }
        None
    }

    /// Asks the model to answer in strict JSON, tolerating a little prose
    /// around it (some OpenAI-compatible providers do not honour
    /// `response_format`) by taking the outermost `{...}` substring.
    fn ask_json(&self, system: &str, user: &str, run: &mut RunState) -> Option<Value> {
        let content = self.chat(system, user, run)?;
        let start = content.find('{')?;
        let end = content.rfind('}')?;
        if end < start {
            return None;
        }
        match serde_json::from_str(&content[start..=end]) {
            Ok(v) => Some(v),
            Err(e) => {
                crate::memory::log(&format!("llm: could not parse JSON reply ({e})"));
                None
            }
        }
    }
}

/// What either nightly step returns: compass directions to discount and an
/// exposure-duration scale. Only ever nudges duration and which directions to
/// avoid -- never the pointing, fibre assignments or declared program.
#[derive(Clone, Debug, Default)]
pub struct NightAdvice {
    pub avoid_directions: Vec<String>,
    pub duration_scale: f64,
}

const COMPASS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];

fn parse_advice(value: &Value) -> NightAdvice {
    let avoid_directions: Vec<String> = value
        .get("avoid_directions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_uppercase())
                .filter(|s| COMPASS.contains(&s.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let duration_scale = value.get("duration_scale").and_then(|v| v.as_f64()).unwrap_or(1.0).clamp(0.7, 1.4);
    NightAdvice { avoid_directions, duration_scale }
}

/// Step A: once per observing night, reads the public forecast notices
/// recorded for tonight plus the current bulletin (see `planner::night_llm_steps`).
pub fn ask_night_advice(client: &LlmClient, run: &mut RunState, context: &str) -> Option<NightAdvice> {
    let system = "You help schedule a telescope survey. The user message describes PUBLIC \
        forecast/bulletin notices for tonight only -- no hidden data. Reply with ONLY a compact \
        JSON object, no prose, no markdown fences: \
        {\"avoid_directions\":[compass codes among N,NE,E,SE,S,SW,W,NW],\"duration_scale\":0.7-1.4}. \
        Avoid directions with bad weather tonight; use a larger duration_scale when the sky looks poor.";
    Some(parse_advice(&client.ask_json(system, context, run)?))
}

/// Step B: once per observing night, reads tonight's live bulletin plus the
/// agent's own hit rate so far (all PUBLIC, from its own prior actions).
pub fn ask_hitrate_advice(client: &LlmClient, run: &mut RunState, context: &str) -> Option<NightAdvice> {
    let system = "You help schedule a telescope survey. The user message describes tonight's PUBLIC \
        bulletin and the agent's own hit rate so far this run -- no hidden data. Reply with ONLY a \
        compact JSON object, no prose, no markdown fences: \
        {\"avoid_directions\":[compass codes among N,NE,E,SE,S,SW,W,NW],\"duration_scale\":0.7-1.4}. \
        A low hit rate suggests longer exposures (duration_scale closer to 1.4); a high one, shorter.";
    Some(parse_advice(&client.ask_json(system, context, run)?))
}

/// Union of `avoid_directions`, average of `duration_scale`; `None` only
/// when both calls returned nothing usable.
pub fn merge_advice(a: Option<NightAdvice>, b: Option<NightAdvice>) -> Option<NightAdvice> {
    match (a, b) {
        (None, None) => None,
        (Some(only), None) | (None, Some(only)) => Some(only),
        (Some(a), Some(b)) => {
            let mut avoid_directions = a.avoid_directions;
            for direction in b.avoid_directions {
                if !avoid_directions.contains(&direction) {
                    avoid_directions.push(direction);
                }
            }
            Some(NightAdvice { avoid_directions, duration_scale: (a.duration_scale + b.duration_scale) / 2.0 })
        }
    }
}
