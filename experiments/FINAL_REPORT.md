# GOSIM Observer Autoresearch Final Report

## Scope and compliance

This study evaluated one card-agnostic planner on L1–L4. Agent logic never reads `local-cards/*/truth/`; truth remains confined to the official runner/scorer. The final Python agent contains no card-ID branch, does not modify the scorer, protocol, validation, or runner, and retains all safety checks.

The V4 runner exposes neither `avoidable_wait` nor `flexible_shortfall` as scored diagnostics. Raw wait actions were inspected separately, and these two fields are reported as unavailable rather than inferred.

## Baselines

The initially discovered implementation contained prohibited L4/D-specific behavior. Its scores are preserved only as an audit baseline and were ineligible for selection:

| Baseline | L1 | L2 | L3 | L4 | Mean | Worst | Robust objective | Required miss |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Raw audit baseline | 4658.331943 | 5037.258991 | 4664.515178 | 4019.606851 | 4594.928241 | 4019.606851 | 4411.981260 | 10 |
| Compliant E002 baseline | 4611.884804 | 4983.757049 | 4660.896439 | 3998.802359 | 4563.835163 | 3998.802359 | 4385.768090 | 11 |

The robust objective is `mean(total_score) - 0.5 * population_std(total_score)`. The official score already includes science value, required-target penalties, uniformity, request rewards, and report settlement, so those terms were not double-counted.

## Best version

E006 is the final best policy. Its original KEEP commit is `a5ee6df`; the final branch HEAD is behaviorally equivalent after rejected experiments were reverted.

| Card | Total | Science/base | Required miss | Uniformity | Report | Request complete | Observe | Raw wait | Invalid action | Invalidated observations |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| L1 | 4728.527875 | 4630.760309 | 2 | -2.232434 | 100 | 1/2 | 610 | 565 | 0 | 0 |
| L2 | 5032.127829 | 4987.437494 | 3 | -5.309664 | 100 | 1/2 | 608 | 648 | 0 | 0 |
| L3 | 4691.631469 | 4646.568489 | 1 | -4.937020 | 0 | 1/2 | 620 | 690 | 0 | 15 |
| L4 | 4160.179962 | 4171.071058 | 2 | -10.891096 | 0 | 1/2 | 572 | 697 | 0 | 198 |

- Mean: 4653.116784
- Worst card: 4160.179962
- Population standard deviation: 313.769938
- Robust objective: 4496.231815
- Required misses: 8 total
- Request misses: 4 of 8
- Invalid actions: 0

`Raw wait` is an action count, not an avoidable-wait diagnosis. `Invalidated observations` are observations later voided by simulator event `V4ST0001`; they are distinct from malformed or rejected agent actions.

Relative to the compliant baseline, E006 improves mean score by 89.281621, worst-card score by 161.377603, robust objective by 110.463725, and reduces required misses from 11 to 8. It also exceeds the ineligible audit baseline by 84.250555 robust-objective points without using card identity.

## Experiment count and decisions

- Valid experiments: 14
- KEEP: 3 (`E002`, `E005`, `E006`)
- REJECT: 11
- Stop condition: eight consecutive experiments without a new best (`E007`–`E014`)

## Five most useful findings

1. **Deterministic public-forecast direction avoidance (E005).** Avoiding directions associated with rain, storm, overcast, haze, and rocket-launch forecasts improved all four cards over the compliant baseline and reduced required misses.
2. **Moderate request-deadline pressure (E006).** A 0.50→1.00 urgency ramp improved the worst card and reduced required misses without sacrificing the robust objective.
3. **Card-agnostic local refinement (E002).** A universal 0.5% minimum gain removed prohibited card-specific behavior while preserving most baseline performance.
4. **The refinement threshold is trajectory-sensitive (E012).** Raising it to 1% catastrophically reduced L2, validating the retained 0.5% value through a strong ablation.
5. **Soft and rare forecast labels carry signal (E013/E014).** Removing either haze or rocket-launch handling degraded the cross-card result, supporting the full E005 forecast set.

## Ineffective or rejected methods

- Simple required-target urgency multipliers produced no behavioral change (E003, E004).
- Unconditional refinement and a 1% refinement threshold hurt robustness (E001, E012).
- Stronger request pressure at the start of the window hurt at least one card without completing more requests (E007, E011).
- Weaker initial request pressure was almost neutral but did not beat the best (E010).
- Forecast-based duration inflation reduced scores (E008); directional choice was safer than pretending forecast conditions imply a fixed exposure multiplier.
- Raising the field-change cost reduced all-card performance and sharply hurt L4 (E009).
- Removing haze or rocket-launch forecast handling degraded multiple cards (E013, E014).

## Generalization risks

- L1–L4 are only four deterministic local scenarios; exact thresholds may be overfit even though no card identity is used.
- Forecast labels may have different calibration or lead time on hidden cards.
- Request completion remains 1/2 on every local card, so the planner still lacks explicit subset-feasibility optimization.
- The local benchmark evaluates the deterministic rule path. It does not establish that LLM planning improves score, latency, stability, or token cost.
- Raw wait count mixes unavoidable closure waits with strategic waits; the runner does not expose an avoidable-wait classification.

## Recommendations for official A/B/C/D cards

- Submit the same card-agnostic planner for every card; do not select behavior from card identity.
- Preserve deterministic forecast-direction avoidance and the 0.50→1.00 request urgency ramp.
- Let public state, visibility, deadlines, weather, and the protocol-provided speed/level determine behavior naturally; do not introduce A/B/C/D branches.
- Keep the rule-only path as the default competition path until an LLM ablation demonstrates repeatable gains under the platform's wall-clock and structured-output constraints.
- Log public observations and action outcomes during official dry runs so forecast calibration and state-resynchronization failures can be diagnosed without using hidden truth.

## Next five research directions

1. Feasibility-aware request selection: choose the smallest high-value subset of request candidates needed for completion rather than applying a uniform request bonus.
2. Rolling-horizon required opportunity counts: estimate remaining visible, weather-compatible slots and prioritize targets with the fewest future opportunities.
3. Program-weather matching: learn or derive expected utility by public forecast band without forcing exposure-duration changes.
4. Adaptive refinement: base the acceptance threshold on measured marginal gain and field density while retaining a card-agnostic rule.
5. Controlled LLM ablation: compare rule-only, forecast interpretation only, and fault-recovery/state-resync only, measuring score, latency, tokens, JSON validity, and fallback rate.

## Literature used

- Rothchild, Stubbs, and Yoachim, *ALTSched: Improved Scheduling for Time-Domain Science with LSST*, arXiv:1903.00531.
- Shetrone et al., *Ten Year Review of Queue Scheduling of the Hobby-Eberly Telescope*, arXiv:0705.3889.
- Foote et al., *Schedule optimization for transiting exoplanet observations with NASA's Pandora SmallSat mission*, arXiv:2305.02285.
