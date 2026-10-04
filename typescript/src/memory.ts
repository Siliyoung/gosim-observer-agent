/**
 * Memory/log: a short rolling history of what happened this run, used for two things --
 *   1. human-readable progress lines on stderr (never stdout: that channel is protocol-only), and
 *   2. compact, public-data-only context for the LLM client (recent notices, recent hit rate),
 *      so prompts stay small and never include anything the agent wasn't told.
 */
import { log } from "./protocol";
import type { Notice } from "./protocol";

export interface NightSummary {
  nightDate: string;
  forecastNotices: Notice[];
  bulletinNotices: Notice[];
}

export class RunMemory {
  private observeCount = 0;
  private hitCount = 0;
  private assignedCount = 0;
  private reportCount = 0;
  private recentForecast: Notice[] = [];
  private startedAt = Date.now();

  recordForecast(notices: Notice[]): void {
    this.recentForecast = notices;
  }

  get lastForecast(): Notice[] {
    return this.recentForecast;
  }

  recordObserveResult(assigned: number, hits: number): void {
    this.observeCount++;
    this.assignedCount += assigned;
    this.hitCount += hits;
  }

  recordReport(): void {
    this.reportCount++;
  }

  /** Fraction of assigned fibres that actually scored, over the whole run so far (0 when no data yet). */
  get overallHitRate(): number | null {
    return this.assignedCount > 0 ? this.hitCount / this.assignedCount : null;
  }

  get counters(): { observes: number; hits: number; assigned: number; reports: number } {
    return { observes: this.observeCount, hits: this.hitCount, assigned: this.assignedCount, reports: this.reportCount };
  }

  logStartup(text: string): void {
    log(`memory: ${text}`);
  }

  logProgress(text: string): void {
    log(text);
  }

  /** Build a small, public-only summary object to hand to the LLM for a night's advice. */
  nightContext(nightDate: string, bulletinNotices: Notice[]): {
    night: string;
    forecast_notices_for_tonight: Notice[];
    current_bulletin_notices: Notice[];
    hit_rate_so_far: number | null;
  } {
    return {
      night: nightDate,
      forecast_notices_for_tonight: this.recentForecast.filter((n) => n.nights?.includes(nightDate)),
      current_bulletin_notices: bulletinNotices,
      hit_rate_so_far: this.overallHitRate,
    };
  }

  elapsedSeconds(): number {
    return (Date.now() - this.startedAt) / 1000;
  }
}
