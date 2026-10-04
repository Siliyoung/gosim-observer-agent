# Baseline

Date: 2026-10-05 (Asia/Shanghai)

## Scope and integrity

- Agent: current Python implementation, evaluated without code changes.
- Cards: public local practice cards L1-L4.
- Command: `python python/tests/evaluate_strategy.py L1 L2 L3 L4` (sequential, deterministic, rule-only; network LLM disabled).
- Official engine integrity: all 16 runner files match `runner/ENGINE_MANIFEST.json` at source commit `db4bddf3cc67fb5f0036d9fb2cf90b9e2b7e8f4a`.
- No file under `local-cards/*/truth/` was opened for this analysis. Hidden truth is consumed only inside the official runner.
- Compliance warning: the starting planner contains a `card_id` branch for `D`/`L4`. This raw baseline is retained for reproducibility, but is not an eligible generalizing best until that branch is removed.

## Results

| Card | Total | Science/base | Required miss | Required penalty | Invalid action | Wait actions | Request miss | Request reward | Uniformity penalty | Report settlement | Targets observed | Observe actions | Invalidated observations |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| L1 | 4658.331943 | 4661.157427 | 2 | -100 | 0 | 569 | 1/2 | 100 | -2.825484 | 0 | 5540 | 613 | 0 |
| L2 | 5037.258991 | 4993.373244 | 3 | -150 | 0 | 644 | 1/2 | 100 | -6.114253 | 100 | 5763 | 605 | 0 |
| L3 | 4664.515178 | 4621.612420 | 1 | -50 | 0 | 687 | 1/2 | 100 | -7.097242 | 0 | 5757 | 617 | 0 |
| L4 | 4019.606851 | 4131.736589 | 4 | -200 | 0 | 700 | 1/2 | 100 | -12.129738 | 0 | 5367 | 579 | 187 |

Aggregate:

- Total score: 18379.712963
- Mean score: 4594.928241
- Worst-card score: 4019.606851
- Population standard deviation: 365.893962
- Robust objective (`mean - 0.5 * population_std`): 4411.981260
- Required misses: 10
- Request misses: 4 of 8 issued requests
- Invalid actions: 0 (all four runs settled normally; invalid actions would terminate the official runner)

## Metric availability

- `science/base score` is the official `sum_best_scores` component.
- `request miss` is derived as `observation_requests_issued - observation_requests_completed`.
- `avoidable wait` is not emitted by the V4 scorer. `wait actions` is recorded as a raw diagnostic, but includes necessary daylight/closure waits and must not be interpreted as avoidable without a counterfactual visibility check.
- `flexible shortfall` is not a concept or field in the current V4 scorer. It is recorded as unavailable rather than inferred.
- The in-process evaluator does not write a workflow envelope, but the score report records a valid settlement and no invalid action exception occurred.

## Scorer analysis

The official total is:

`sum_best_scores - required_cost - uniformity_cost + request_reward + report_settlement`.

For each target, only the best valid exposure contribution counts. Required completion uses the maximum raw completion factor, not the program multiplier. Uniformity is a Jain-index penalty over RA-band completion fractions. Request rewards are all-or-nothing at their deadlines; the current configuration has no extra miss penalty. Data-loss invalidations cause the ledger to recompute valid best scores and completion factors.

Because these penalties and rewards are already included in `total`, the experiment-selection metric does not add them again. The primary scalar metric is `mean(L1..L4) - 0.5 * std(L1..L4)`; mean, worst-card score, required misses, request misses, invalid actions, and wait counts remain mandatory diagnostics. A candidate cannot be kept solely because this scalar rises if it causes a material worst-card regression or violates the generalization constraints.

## Test note

The initially suggested discovery command failed at import time because the repository root does not put `python/` on `sys.path`. This is a test-entry configuration issue, not a failed strategy assertion. The autoresearch guard will run the same tests with an explicit Python module path.
