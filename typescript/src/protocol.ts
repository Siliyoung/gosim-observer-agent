/**
 * participant-agent-protocol-v4: JSON Lines transport.
 *
 * The platform writes one JSON object per line on our stdin; we answer with exactly one
 * JSON object per line on stdout for every `decision_request`. Logs must never touch stdout.
 */

export const PROTOCOL_VERSION = "participant-agent-protocol-v4";

export interface SiteConfig {
  name: string;
  latitude_deg: number;
  longitude_deg: number;
  utc_offset_hours: number;
  sun_altitude_limit_deg: number;
  minimum_altitude_deg: number;
}

export interface NightConfig {
  night_id: string;
  night_date: string;
  observing_start_utc: string;
  observing_end_utc: string;
  slot_count: number;
}

export interface SurveyConfig {
  start_utc: string;
  end_utc: string;
  slot_seconds: number;
  nights: NightConfig[];
}

export interface InstrumentConfig {
  n_fibers: number;
  grid_side: number;
  fiber_area_deg2: number;
  gap_deg: number;
  glass_side_deg: number;
  pitch_deg: number;
  fov_side_deg: number;
  layout: string;
  exposure: { min_duration_seconds: number; max_duration_seconds: number };
}

export interface ProgramConfig {
  bands: { DARK: number; BRIGHT: number };
  multipliers: { DARK: number; BRIGHT: number; BACKUP: number };
  mismatch_multiplier: number;
}

export interface LunarModelConfig {
  angular_decay_scale_deg: number;
  altitude_exponent: number;
  maximum_penalty: number;
}

export interface ScoringConfig {
  schema_version: string;
  q0: number;
  flux_zero_point: number;
  exposure_zero_point_seconds: number;
  airmass_exponent: number;
  lunar_model: LunarModelConfig;
  program: ProgramConfig;
  required: { penalty_per_missing: number; observed_factor_threshold: number };
  uniformity: { weight: number; ra_band_width_deg: number; observed_factor_threshold: number };
  reporting: {
    correct_reward: number;
    false_penalty: number;
    false_report_free_allowance: number;
    max_consecutive_reports: number;
  };
  [key: string]: unknown;
}

export interface FootprintComponent {
  component_id: string;
  vertices: [number, number][];
}

export interface TargetsTable {
  columns: string[];
  rows: (string | number | boolean)[][];
}

export interface LimitsConfig {
  global_wallclock_seconds: number;
  max_consecutive_reports: number;
  response_max_bytes: number;
  decision_timeout: string;
}

export interface InitializePayload {
  schema_version: string;
  task_card: { card_id: string; scenario_slug?: string; phase?: string };
  site: SiteConfig;
  survey: SurveyConfig;
  instrument: InstrumentConfig;
  scoring: ScoringConfig;
  footprint: FootprintComponent[];
  targets: TargetsTable;
  limits: LimitsConfig;
}

export interface Notice {
  event_kind: string;
  direction: string;
  nights?: string[];
}

export interface BulletinMessage {
  record_type: "bulletin";
  slot_id: string;
  night_id: string;
  issued_at_utc: string;
  initial: boolean;
  notices: Notice[];
}

export interface ForecastMessage {
  record_type: "forecast";
  issued_at_utc: string;
  coverage_start_utc: string;
  coverage_end_utc: string;
  notices: Notice[];
}

export interface ReportResultMessage {
  record_type: "report_result";
  issued_at_utc: string;
  correct: boolean;
  repaired: boolean;
  score_delta: number;
}

export interface StateResyncMessage {
  record_type: "state_resync";
  issued_at_utc: string;
  trigger_event_id: string;
  invalidated_window: {
    action_count_at_trigger: number;
    action_index_start: number;
    action_index_end_exclusive: number;
    window_start_fraction: number;
    window_end_fraction: number;
    window_max_fraction: number;
  };
  observed_target_ids: string[];
  best_scores: { target_id: string; best_score: number }[];
}

export type PlatformMessage = BulletinMessage | ForecastMessage | ReportResultMessage | StateResyncMessage;

export interface ObserveHit {
  target_id: string;
  score: number;
}

export type LastResult =
  | null
  | { action: "wait" }
  | { action: "observe"; observe_index: number; assigned_count: number; hit_count: number; hits: ObserveHit[] }
  | { action: "report"; correct: boolean; repaired: boolean; score_delta: number };

export interface DecisionRequestPayload {
  schema_version: string;
  now_utc: string;
  survey_end_utc: string;
  observe_action_index: number;
  running_total: number;
  wallclock: { elapsed_seconds: number; remaining_seconds: number };
  latest_bulletin: BulletinMessage | null;
  latest_forecast: ForecastMessage | null;
  new_messages: PlatformMessage[];
  last_result: LastResult;
}

export interface InitializeEnvelope {
  protocol_version: string;
  message_type: "initialize";
  payload: InitializePayload;
}

export interface DecisionRequestEnvelope {
  protocol_version: string;
  message_type: "decision_request";
  decision_sequence: number;
  payload: DecisionRequestPayload;
}

export interface FinishEnvelope {
  protocol_version: string;
  message_type: "finish";
  payload: {
    schema_version: string;
    termination_reason: string;
    decisions: number;
    observe_actions: number;
    last_decision_sequence: number;
    grace_seconds: number;
  };
}

export type PlatformEnvelope = InitializeEnvelope | DecisionRequestEnvelope | FinishEnvelope;

export type ObserveAction = {
  action: "observe";
  pointing: { alt_deg: number; az_deg: number };
  assignments: Record<string, string>;
  duration_seconds: number;
  program?: "DARK" | "BRIGHT" | "BACKUP";
  reason?: string;
  decision_source?: string;
};

export type WaitAction =
  | { action: "wait"; duration_seconds: number; reason?: string; decision_source?: string }
  | { action: "wait"; until_utc: string; reason?: string; decision_source?: string };

export type ReportAction = { action: "report"; reason?: string; decision_source?: string };
export type FinishAction = { action: "finish"; reason?: string; decision_source?: string };

export type DecisionAction = ObserveAction | WaitAction | ReportAction | FinishAction;

/** Write one assistant log line to stderr. Never write to stdout: that channel is protocol-only. */
export function log(text: string): void {
  process.stderr.write(text + "\n");
}

/** Build and serialize exactly one decision_response line, including only the fields that action uses. */
export function encodeDecisionResponse(decisionSequence: number, action: DecisionAction): string {
  const envelope: Record<string, unknown> = {
    protocol_version: PROTOCOL_VERSION,
    message_type: "decision_response",
    decision_sequence: decisionSequence,
    action: action.action,
  };
  if (action.action === "observe") {
    envelope.pointing = action.pointing;
    envelope.assignments = action.assignments;
    envelope.duration_seconds = action.duration_seconds;
    envelope.program = action.program ?? "BACKUP";
  } else if (action.action === "wait") {
    if ("duration_seconds" in action) {
      envelope.duration_seconds = action.duration_seconds;
    } else {
      envelope.until_utc = action.until_utc;
    }
  }
  if (action.reason !== undefined) envelope.reason = action.reason;
  if (action.decision_source !== undefined) envelope.decision_source = action.decision_source;
  return JSON.stringify(envelope);
}

export function writeDecisionResponse(decisionSequence: number, action: DecisionAction): void {
  const line = encodeDecisionResponse(decisionSequence, action);
  process.stdout.write(line + "\n");
}
