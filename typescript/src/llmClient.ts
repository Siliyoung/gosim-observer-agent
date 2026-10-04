/**
 * LLM advice via an OpenAI-compatible chat-completions endpoint, using Node's built-in `fetch`
 * (no SDK dependency). Defaults to the Kimi Coding Plan endpoint and model; point OPENAI_BASE_URL
 * at any other OpenAI-compatible provider to use that instead.
 *
 *   OPENAI_BASE_URL   endpoint base URL; defaults to https://api.kimi.com/coding/v1
 *                      (use https://api.kimi.ai/coding/v1 for the overseas endpoint)
 *   OPENAI_API_KEY    credential for the endpoint (KIMI_API_KEY also accepted)
 *   OPENAI_MODEL      model id; defaults to "k3"
 *
 * Each call has a short timeout and the whole run has a small total time budget; a failed or
 * timed-out call is retried a bounded number of times, and if it still doesn't come back the
 * caller gets `null` and uses its own rule-based decision for that one step.
 */
import { log } from "./protocol";

export interface LLMConfig {
  enabled: boolean;
  baseUrl: string;
  apiKey: string;
  model: string;
}

const DEFAULT_BASE_URL = "https://api.kimi.com/coding/v1";
const DEFAULT_MODEL = "k3";

function readConfig(): LLMConfig {
  const baseUrl = (process.env.OPENAI_BASE_URL ?? DEFAULT_BASE_URL).trim().replace(/\/+$/, "");
  const apiKey = (process.env.OPENAI_API_KEY ?? process.env.KIMI_API_KEY ?? "").trim();
  const model = (process.env.OPENAI_MODEL ?? "").trim() || DEFAULT_MODEL;
  return { enabled: Boolean(baseUrl && apiKey), baseUrl, apiKey, model };
}

/** Short, user-facing config error, or null when a key is configured. */
export function configError(): string | null {
  return readConfig().apiKey ? null : "missing API key: set OPENAI_API_KEY";
}

export class LLMAdvisor {
  readonly enabled: boolean;
  private readonly config: LLMConfig;
  private readonly timeoutSeconds: number;
  private readonly budgetSeconds: number;
  private readonly maxCalls: number;
  private readonly maxAttemptsPerCall: number;
  private spentSeconds = 0;
  calls = 0;

  constructor(timeoutSeconds = 12.0, budgetSeconds = 300.0, maxCalls = 100, maxAttemptsPerCall = 3) {
    this.config = readConfig();
    this.enabled = this.config.enabled;
    this.timeoutSeconds = timeoutSeconds;
    this.budgetSeconds = budgetSeconds;
    this.maxCalls = maxCalls;
    this.maxAttemptsPerCall = maxAttemptsPerCall;
  }

  get status(): string {
    return this.enabled ? "on" : "off";
  }

  private async chat(system: string, user: string, wallclockLeftSeconds: number): Promise<Record<string, unknown> | null> {
    if (!this.enabled) return null;
    for (let attempt = 1; attempt <= this.maxAttemptsPerCall; attempt++) {
      if (this.calls >= this.maxCalls) return null;
      const timeoutMs = Math.min(this.timeoutSeconds, this.budgetSeconds - this.spentSeconds, Math.max(0, wallclockLeftSeconds - 60.0)) * 1000;
      if (timeoutMs < 2000) return null;

      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), timeoutMs);
      const started = Date.now();
      this.calls++;
      try {
        const response = await fetch(`${this.config.baseUrl}/chat/completions`, {
          method: "POST",
          headers: { "Content-Type": "application/json", Authorization: `Bearer ${this.config.apiKey}` },
          body: JSON.stringify({
            model: this.config.model,
            messages: [
              { role: "system", content: system },
              { role: "user", content: user },
            ],
            temperature: 0,
            max_tokens: 200,
          }),
          signal: controller.signal,
        });
        if (!response.ok) throw new Error(`http ${response.status}`);
        const data = (await response.json()) as { choices?: { message?: { content?: string } }[] };
        const text = data.choices?.[0]?.message?.content ?? "";
        const match = /\{[\s\S]*\}/.exec(text);
        return match ? (JSON.parse(match[0]) as Record<string, unknown>) : null;
      } catch (exc) {
        const name = exc instanceof Error ? exc.name || exc.constructor.name : "Error";
        log(`llm: call failed (${name}); attempt ${attempt}/${this.maxAttemptsPerCall}`); // never log the key
      } finally {
        clearTimeout(timer);
        this.spentSeconds += (Date.now() - started) / 1000;
      }
    }
    log("llm: call did not succeed; using the rule-based decision for this step");
    return null;
  }

  /** Shared parsing for the {avoid_directions, duration_scale} answer shape. */
  private parseAdvice(answer: Record<string, unknown>): { avoidDirections: string[]; durationScale: number } {
    const directions = new Set(["N", "NE", "E", "SE", "S", "SW", "W", "NW"]);
    const avoid = Array.isArray(answer.avoid_directions)
      ? [...new Set(answer.avoid_directions.map((d) => String(d).toUpperCase()).filter((d) => directions.has(d)))].sort()
      : [];
    let scale = 1.0;
    const raw = Number(answer.duration_scale);
    if (Number.isFinite(raw)) scale = Math.min(1.4, Math.max(0.7, raw));
    return { avoidDirections: avoid, durationScale: scale };
  }

  /** -> {avoidDirections, durationScale} or null. One short call per night, from the forecast. */
  async nightPlan(
    nightDate: string,
    forecastNotices: unknown,
    bulletinNotices: unknown,
    wallclockLeftSeconds: number
  ): Promise<{ avoidDirections: string[]; durationScale: number } | null> {
    const system =
      "You help schedule a telescope survey. Reply with one JSON object only: " +
      '{"avoid_directions": [compass codes among N,NE,E,SE,S,SW,W,NW], "duration_scale": number 0.7-1.4}. ' +
      "Avoid directions with bad weather tonight; use a larger duration_scale when the sky is poor.";
    const user = JSON.stringify({ night: nightDate, forecast_notices_for_tonight: forecastNotices, current_bulletin_notices: bulletinNotices });
    const answer = await this.chat(system, user, wallclockLeftSeconds);
    return answer ? this.parseAdvice(answer) : null;
  }

  /**
   * -> {avoidDirections, durationScale} or null. A second, separate call each night that reacts to
   * the latest public bulletin and the run's own hit rate so far -- distinct from nightPlan, which
   * only looks at the forecast. Catches weather or performance that changed since nightPlan ran.
   */
  async bulletinCheckIn(
    nightDate: string,
    bulletinNotices: unknown,
    hitRateSoFar: number | null,
    wallclockLeftSeconds: number
  ): Promise<{ avoidDirections: string[]; durationScale: number } | null> {
    const system =
      "You monitor an in-progress telescope survey. Reply with one JSON object only: " +
      '{"avoid_directions": [compass codes among N,NE,E,SE,S,SW,W,NW], "duration_scale": number 0.7-1.4}. ' +
      "Avoid directions the current bulletin flags as bad right now; raise duration_scale if the recent hit " +
      "rate is low (fibres are missing their targets), lower it if the hit rate is high.";
    const user = JSON.stringify({ night: nightDate, current_bulletin_notices: bulletinNotices, hit_rate_so_far: hitRateSoFar });
    const answer = await this.chat(system, user, wallclockLeftSeconds);
    return answer ? this.parseAdvice(answer) : null;
  }

  /** -> true (report), false (do not report), or null (no opinion: keep the rule's decision). */
  async confirmReport(evidence: unknown, wallclockLeftSeconds: number): Promise<boolean | null> {
    const system =
      "You check telescope data quality. A false instrument-fault report costs points, a correct one " +
      'earns points. Reply with one JSON object only: {"report": true|false}.';
    const answer = await this.chat(system, JSON.stringify(evidence), wallclockLeftSeconds);
    return answer && typeof answer.report === "boolean" ? (answer.report as boolean) : null;
  }
}
