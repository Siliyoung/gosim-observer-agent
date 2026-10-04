#!/usr/bin/env node
/**
 * Entry point: a survey agent (participant-agent-protocol-v4). Reads one JSON object per line on
 * stdin, writes one JSON object per line on stdout, logs to stderr only.
 *
 *   initialize        -> build state + planner (catalogue, sky index, night windows)
 *   decision_request   -> answer with observe / wait / report / finish
 *   finish             -> print a one-line summary to stderr and exit
 *
 * Strategy in one paragraph: sleep through the day with one `wait` + `until_utc`; at night point at
 * the most urgent visible target, fill the other fibres with the most valuable neighbours, and expose
 * just long enough (planner.ts). Close the shutter (wait one slot) while a bulletin says rain or storm
 * over the whole sky. Each night, ask the LLM advisor twice -- once from the forecast (nightPlan), once
 * from the current bulletin and the run's own hit rate so far (bulletinCheckIn) -- for weather-avoidance
 * and exposure-scale advice, and merge the two. Separately, confirm a suspected instrument fault with
 * the advisor before reporting it (at most twice per run). Every branch is wrapped so a bug here never
 * crashes the run: on any error we fall back to a safe wait.
 */
import * as readline from "node:readline";
import {
  DecisionAction,
  DecisionRequestEnvelope,
  FinishEnvelope,
  InitializeEnvelope,
  PlatformEnvelope,
  log,
  writeDecisionResponse,
} from "./protocol";
import { AgentState } from "./state";
import { Planner } from "./planner";
import { LLMAdvisor, configError } from "./llmClient";
import { RunMemory } from "./memory";
import { deterministicFallback, validateAction } from "./validate";
import { formatUtc, parseUtc } from "./skymath";

const PROTOCOL = "participant-agent-protocol-v4";

const REPORT_DROP = 0.62; // report when recent clean-sky quality falls below 62% of the earlier level
const REPORT_CONFIRMATIONS = 3; // ... on this many checks in a row, on different nights (the drop must persist)
const REPORT_SPACING_HOURS = 6.0;
const MAX_REPORTS = 2;

class Agent {
  private readonly state: AgentState;
  private readonly planner: Planner;
  private readonly advisor: LLMAdvisor;
  private readonly memory: RunMemory;
  private readonly start: Date;

  private nightSeen: number | null = null;
  private reports = 0;
  private lastReportHours = -1e9;
  private suspicion: number[] = [];
  private observes = 0;
  private consecutiveReports = 0;

  constructor(payload: InitializeEnvelope["payload"]) {
    const started = Date.now();
    this.state = new AgentState(payload);
    this.planner = new Planner(this.state);
    this.advisor = new LLMAdvisor();
    this.memory = new RunMemory();
    this.start = parseUtc(payload.survey.start_utc);
    const requiredCount = this.state.required.filter(Boolean).length;
    this.memory.logStartup(
      `${this.state.ids.length} targets, ${requiredCount} required, ${this.state.nights.length} nights; ` +
        `init ${(Date.now() - started) / 1000}s; llm=${this.advisor.status}`
    );
  }

  get observeCount(): number {
    return this.observes;
  }
  get reportCount(): number {
    return this.reports;
  }
  get llmCalls(): number {
    return this.advisor.calls;
  }

  async respond(payload: DecisionRequestEnvelope["payload"]): Promise<DecisionAction> {
    const now = parseUtc(payload.now_utc);
    const hours = (now.getTime() - this.start.getTime()) / 3_600_000;
    const st = this.state;

    for (const message of payload.new_messages) {
      if (message.record_type === "forecast") this.memory.recordForecast(message.notices);
    }
    st.onMessages(payload.new_messages, payload.latest_bulletin);
    st.onResult(payload.last_result, hours);
    if (payload.last_result?.action === "observe") {
      this.memory.recordObserveResult(payload.last_result.assigned_count, payload.last_result.hit_count);
    }
    this.pace(payload, now);

    const night = st.currentNight(now);
    if (night === null) {
      const next = st.nextNightStart(now);
      if (next === null) return { action: "finish", reason: "no observing night left" };
      return { action: "wait", until_utc: formatUtc(next), reason: "daytime: sleep until the next night" };
    }
    if (this.nightSeen !== night.index) {
      this.nightSeen = night.index;
      await this.nightAdvice(night.start, payload);
    }
    if ((night.end.getTime() - now.getTime()) / 1000 < st.minExposure) {
      const next = st.nextNightStart(now);
      if (next === null) return { action: "finish", reason: "survey over" };
      return { action: "wait", until_utc: formatUtc(next), reason: "night ending" };
    }
    if (this.planner.siteClosed()) {
      return { action: "wait", duration_seconds: this.toNextSlot(now, night.start), reason: "bulletin: rain/storm over the whole sky" };
    }
    const report = await this.maybeReport(hours, payload);
    if (report !== null) return report;

    const action = this.planner.plan(now, night.end, night.index, hours);
    if (action === null) {
      return { action: "wait", duration_seconds: this.toNextSlot(now, night.start), reason: "nothing useful is up" };
    }
    this.observes++;
    return { ...action, reason: `${Object.keys(action.assignments).length} fibres, program ${action.program}` };
  }

  private toNextSlot(now: Date, nightStart: Date): number {
    const slot = this.state.slotSeconds;
    const into = ((now.getTime() - nightStart.getTime()) / 1000) % slot;
    return Math.round(Math.max(60, Math.min(3600, slot - into)));
  }

  /** Do less work per decision when the wall clock is short for the nights still to come. */
  private pace(payload: DecisionRequestEnvelope["payload"], now: Date): void {
    const remainingWall = payload.wallclock?.remaining_seconds ?? 1e9;
    let nightSeconds = 0;
    for (const { start, end } of this.state.nights) {
      if (end > now) nightSeconds += Math.max(0, (end.getTime() - Math.max(start.getTime(), now.getTime())) / 1000);
    }
    const decisionsLeft = Math.max(1.0, nightSeconds / 700.0);
    const perDecision = remainingWall / decisionsLeft;
    const level = perDecision > 0.12 ? 0 : perDecision > 0.04 ? 1 : 2;
    if (level !== this.state.fastLevel) {
      log(`agent: pace level ${level} (${Math.round(perDecision * 1000)} ms per decision left)`);
      this.state.fastLevel = level;
    }
  }

  private async nightAdvice(nightStart: Date, payload: DecisionRequestEnvelope["payload"]): Promise<void> {
    this.state.extraAvoid = new Set();
    this.state.durationScale = 1.0;
    if (!this.advisor.enabled) return;
    const nightDate = new Date(nightStart.getTime() - 12 * 3_600_000).toISOString().slice(0, 10);
    const tonight = this.memory.lastForecast.filter((n) => n.nights?.includes(nightDate));
    const bulletin = payload.latest_bulletin?.notices ?? [];
    const left = payload.wallclock?.remaining_seconds ?? 0;

    const plan = await this.advisor.nightPlan(nightDate, tonight, bulletin, left);
    if (plan) log(`llm night ${nightDate}: avoid ${[...plan.avoidDirections]} duration x${plan.durationScale.toFixed(2)}`);

    const checkIn = await this.advisor.bulletinCheckIn(nightDate, bulletin, this.memory.overallHitRate, left);
    if (checkIn) {
      log(`llm bulletin ${nightDate}: avoid ${[...checkIn.avoidDirections]} duration x${checkIn.durationScale.toFixed(2)}`);
    }

    if (!plan && !checkIn) return;
    const avoid = new Set([...(plan?.avoidDirections ?? []), ...(checkIn?.avoidDirections ?? [])]);
    const scales = [plan?.durationScale, checkIn?.durationScale].filter((s): s is number => s !== undefined);
    const scale = scales.reduce((sum, s) => sum + s, 0) / scales.length;
    this.state.extraAvoid = avoid;
    this.state.durationScale = Math.min(1.4, Math.max(0.7, scale));
  }

  /**
   * Report only when clean-sky quality dropped a lot and stayed low on three different nights, and
   * saturated hits declared DARK do not show that the sky band dropped too (that would be weather).
   */
  private async maybeReport(hours: number, payload: DecisionRequestEnvelope["payload"]): Promise<DecisionAction | null> {
    const st = this.state;
    st.forceProgram = null;
    if (this.reports >= MAX_REPORTS || hours - this.lastReportHours < 24.0) return null;
    const evidence = st.faultEvidence();
    const threshold = this.reports === 0 ? REPORT_DROP : REPORT_DROP - 0.07;
    if (evidence === null || evidence.drop >= threshold) {
      this.suspicion = [];
      return null;
    }
    if (evidence.dark_checks < 6) {
      st.forceProgram = "DARK"; // diagnostic: ask the sky which band it is in
    } else if (evidence.dark_matched < 0.5 * evidence.dark_checks) {
      this.suspicion = []; // the sky band dropped too: weather, not the instrument
      return null;
    }
    if (this.suspicion.length > 0 && hours - this.suspicion[this.suspicion.length - 1]! < REPORT_SPACING_HOURS) return null;
    this.suspicion.push(hours);
    if (this.suspicion.length < REPORT_CONFIRMATIONS) return null;
    this.suspicion = [];
    const verdict = await this.advisor.confirmReport(evidence, payload.wallclock?.remaining_seconds ?? 0);
    if (verdict === false) {
      log(`agent: report vetoed by the model at ${payload.now_utc} (${JSON.stringify(evidence)})`);
      this.lastReportHours = hours;
      return null;
    }
    this.reports++;
    this.memory.recordReport();
    this.lastReportHours = hours;
    st.forgetQualityHistory();
    log(`agent: report instrument fault at ${payload.now_utc} evidence=${JSON.stringify(evidence)}`);
    return { action: "report", reason: `quality dropped to ${(evidence.drop * 100).toFixed(0)}% of the earlier level`, decision_source: verdict ? "llm-confirmed" : "rule" };
  }

  /** Validate an action against the public limits; on failure, fall back to a safe deterministic one. */
  checkedAction(candidate: DecisionAction, now: Date): DecisionAction {
    const result = validateAction(candidate, this.state, this.consecutiveReports);
    let action = candidate;
    if (!result.ok) {
      log(`agent: rejected own ${candidate.action} action (${result.reason}); using a deterministic fallback`);
      action = deterministicFallback(this.state, now, `validation failed: ${result.reason}`);
    }
    this.consecutiveReports = action.action === "report" ? this.consecutiveReports + 1 : 0;
    return action;
  }
}

async function main(): Promise<number> {
  const configProblem = configError();
  if (configProblem) {
    log(`agent: ${configProblem}`);
    return 1;
  }

  const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
  let agent: Agent | null = null;

  for await (const rawLine of rl) {
    const line = rawLine.trim();
    if (!line) continue;
    let message: PlatformEnvelope;
    try {
      message = JSON.parse(line) as PlatformEnvelope;
    } catch {
      log("agent: received a line that is not valid JSON; ignoring it");
      continue;
    }
    if (message.protocol_version !== PROTOCOL) {
      log(`agent: unexpected protocol ${message.protocol_version}`);
    }

    if (message.message_type === "initialize") {
      try {
        agent = new Agent((message as InitializeEnvelope).payload);
      } catch (exc) {
        log(`agent: failed to initialize: ${String(exc)}`);
      }
    } else if (message.message_type === "decision_request") {
      const request = message as DecisionRequestEnvelope;
      let action: DecisionAction;
      const now = parseUtc(request.payload.now_utc);
      try {
        if (!agent) throw new Error("decision_request received before initialize");
        action = await agent.respond(request.payload);
      } catch (exc) {
        log(`agent: error ${exc instanceof Error ? exc.constructor.name : "Error"}: ${String(exc)}; waiting one slot`);
        action = { action: "wait", duration_seconds: 900, reason: "internal error" };
      }
      action = agent ? agent.checkedAction(action, now) : action;
      if (action.decision_source === undefined) {
        action = { ...action, decision_source: agent && agent.llmCalls > 0 ? "llm-advised" : "deterministic" };
      }
      writeDecisionResponse(request.decision_sequence, action);
    } else if (message.message_type === "finish") {
      const finish = message as FinishEnvelope;
      const observes = agent?.observeCount ?? 0;
      const reports = agent?.reportCount ?? 0;
      log(`agent finished: termination_reason=${finish.payload.termination_reason} observes=${observes} reports=${reports}`);
    }
  }
  return 0;
}

main()
  .then((code) => process.exit(code))
  .catch((exc) => {
    log(`agent: fatal error: ${String(exc)}`);
    process.exit(1);
  });
