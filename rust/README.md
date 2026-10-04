# rust-agent

A complete, multi-file Rust reference agent for the GOSIM Agent Observer
Challenge (`participant-agent-protocol-v4`). It speaks JSON Lines on
stdin/stdout exactly as the participant guide describes (protocol
section 8): one `initialize`, then one `decision_response` per
`decision_request`, until a closing `finish`.

It plans with public geometry and scoring formulas only, closely following
the project's sibling TypeScript reference agent
(`examples/typescript`, which was built and tuned against the same public
protocol) -- an anchor search over candidate pointings, a night calendar so
it sleeps cleanly through the day, and a learned sky-quality scale -- then
lets an LLM nudge a couple of knobs on top each night (see "LLM usage"
below). On the local practice card `v4-practice-own/alpha` it scores higher
and runs faster than that TypeScript baseline (see "Verifying it yourself"
below).

Needs a chat-completions API key to start: see "LLM usage".

中文说明见 [README_ZH.md](README_ZH.md)。

## What it does

1. Parses `initialize.payload` (site, instrument, scoring config, the
   published night calendar and targets) into a typed snapshot
   (`state::Config`): per-target visibility windows (`max_hour_angle_deg`,
   first/last night) and a declination-band spatial index for fast
   "what else is nearby" lookups.
2. During the day, or between nights, sleeps in one hop straight to the next
   observing night (`wait` with `until_utc`) instead of polling every
   exposure-length `wait` -- the night calendar is public, so there is
   nothing to gain by checking more often.
3. Within a night, ranks visible, not-yet-done targets (`planner::value`):
   `required` targets not yet "safe" get a bonus, since missing one costs
   real points at settlement. For the best few candidates, it tries **every
   fibre as the pointing centre** for that candidate (`scoring::shift_altaz`),
   fills the rest of the field with whichever nearby targets land on their
   own glass, and keeps the field that captures the most total expected value
   -- not just whatever happens to overlap the single best target's own
   pointing.
4. Picks the exposure length with the best expected gain per second (tried
   against ten candidate durations), and the program (`DARK`/`BRIGHT`/
   `BACKUP`) most of the assigned targets are expected to match.
5. Learns a sky-quality scale from its own `last_result.hits` scores
   (`memory::Memory::scale`, a median over recent samples) -- the guide's own
   suggestion for working around the hidden per-slot weather -- and backs out
   each hit's true completion factor from the declared-vs-mismatch program
   multiplier it actually got paid.
6. Asks an LLM for two bounded nudges each observing night (see "LLM usage" below).
7. Validates every outgoing response against the protocol's hard rules
   (`validate.rs`) before printing it, and substitutes a guaranteed-legal
   minimum-length `wait` if anything about its own plan looks wrong.

## Module map

| File | Responsibility |
|---|---|
| `src/main.rs` | Reads `initialize`, then runs the decision loop; logs to stderr. |
| `src/protocol.rs` | Serde types for the wire format; permissive JSON-Lines read/write. |
| `src/state.rs` | One-time config snapshot from `initialize`: catalogue, night calendar, visibility windows, spatial index. |
| `src/memory.rs` | What the agent has learned from its own feedback (progress, learned scale, weather notices), and the stderr logger. |
| `src/planner.rs` | Turns one `decision_request` into one `decision_response`: the anchor search and duration/program choice. |
| `src/llm.rs` | OpenAI-compatible chat client (default: Kimi Coding Plan), with retries and a run-wide budget. |
| `src/scoring.rs` | Public sky geometry + scoring formulas (no scenario data). |
| `src/validate.rs` | Protocol-rule validation and the deterministic safe fallback. |

## Building and running

```sh
cargo build --release
OPENAI_API_KEY=<your-key> ./target/release/rust-agent < some_session.jsonl
```

Building needs no scenario data and no key. Running does need a key -- see
"LLM usage" below for where it comes from and what happens without one.
Once running, the agent only ever sees whatever `initialize`/`decision_request`
messages arrive on stdin; no scenario data ships with this example.

### Packaging for submission

`observer.project.json` tells the platform how to build and run this project:

```json
{
  "build": [["cargo", "build", "--release", "--locked"]],
  "run": ["./target/release/rust-agent"]
}
```

Build time is not charged against the survey's wall-clock budget (that clock
starts at the first `decision_request`), so the release profile favours
reasonable compile times (`opt-level = 2`) over maximum optimization.

### Verifying it yourself

Point any protocol-compatible local test harness (the platform itself, or
your own) at the compiled binary -- it takes a plain command list for the
agent process, so it runs a compiled binary just as well as a script. Point
`OPENAI_BASE_URL` at a local OpenAI-compatible mock (or a real key) to try it
against a public practice card without a live provider in the loop.

## LLM usage

Configuration (see `.env.example`):

- **Key**: `OPENAI_API_KEY`, or `KIMI_API_KEY` as an alternative name for a
  Kimi-only key. Neither set -> the agent logs `missing API key: set
  OPENAI_API_KEY` and exits before reading anything from stdin.
- **Endpoint/model**: default to the Kimi Coding Plan
  (https://www.kimi.com/code/docs/en/), an OpenAI-compatible chat-completions
  API: base URL `https://api.kimi.com/coding/v1`, model `k3`. Set
  `OPENAI_BASE_URL` / `OPENAI_MODEL` to point at that endpoint's overseas
  alternative (`https://api.kimi.ai/coding/v1`) or any other OpenAI-compatible
  provider instead.

Every observing night, two calls run and their advice is merged (union of
avoided compass directions, average of the duration scale):

1. **Night advice** (`llm::ask_night_advice`): reads the public forecast
   notices recorded for tonight plus the current bulletin, and asks for
   compass directions to discount and an exposure-duration scale (0.7-1.4).
2. **Hit-rate advice** (`llm::ask_hitrate_advice`): reads that same night's
   live bulletin plus the agent's own hit rate so far this run, and asks for
   the same two things.

Both calls only ever *nudge* the plan's exposure duration and which
directions it discounts; neither chooses the pointing, fibre assignments, or
the declared program. A call that fails (connect/timeout error, non-JSON
reply, a reply that doesn't parse as the expected object) is retried up to 3
times; if every attempt fails, that night's plan uses its own default numbers
for that one step, and the next night's calls run again as normal.

Caps, all overridable via `.env`:

- `LLM_TIMEOUT_SECONDS` (default 12) per attempt.
- `LLM_BUDGET_SECONDS` (default 300) of real call time across the whole run.
- `LLM_MAX_CALLS` (default 100) calls across the whole run.
- The planner also stops attempting LLM calls once less than 30s of the
  run's wall-clock budget remains, and proactively sends `finish` once less
  than 10s remains, rather than risk being force-killed mid-decision.

## A note on determinism

Two internal maps (fibre assignments within one field, and the targets
"pending" a result between decisions) are intentionally `BTreeMap`, not
`HashMap`: a plain `HashMap`'s iteration order is randomized per process, and
when two candidates tie, that randomness leaked into which one the planner
kept -- producing a materially different trajectory (and score) on separate
runs of the identical binary against the identical card, even before any LLM
reply varies the plan further. Fixing that means the deterministic core
(geometry, scoring, fibre search) always makes the same choice for the same
inputs -- same card, same LLM replies, same score, every time.

## Security note

This example contains **no scenario data**: no truth files, no
`v4_bulletins.jsonl` / `v4_forecasts.jsonl`, no weather tables, nothing copied
from any task card's `public/`, `config/` or `truth/` directories. The only
bulletins/forecasts the agent ever sees are the ones the real protocol
delivers at runtime in `decision_request.payload.new_messages`. Verify with:

```sh
grep -rniE "truth|bulletin|forecasts\.jsonl" src/ | grep -v record_type
```

(The remaining hits are comments/prompts referring to the live `bulletin`/
`forecast` *message types* the protocol sends, and to the fact that no truth
file is ever read -- not to any bundled data.)

## Known limitations

- The anchor search tries a bounded number of candidates and fibre
  placements (`planner::ANCHORS`, `ANCHOR_POOL`) per decision, not every
  combination -- a good trade-off at 10,000+ targets, but not exhaustive.
- The sidereal-time and solar/lunar formulas are the guide's own low-order
  approximations (good to a few arcseconds), not a full ephemeris.
- The opportunistic `report` heuristic (`planner::maybe_report`) is
  deliberately conservative (a long, fully-assigned miss streak) and may
  never fire on an easy task card -- that is by design, since a wrong guess
  costs `scoring.reporting.false_penalty`.

## License / Citation

Task cards, simulated data, evaluation code and this example project are licensed under
[CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/) (attribution, non-commercial);
please cite the GOSIM 2026 Agentic Observer Challenge. Your own agent code is not restricted
by this. See `LICENSE.md` for details.
