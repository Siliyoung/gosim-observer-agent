# GOSIM Agent Observer Challenge -- TypeScript (Node.js) example agent

A complete, minimal-dependency TypeScript agent for `participant-agent-protocol-v4`: it runs a
virtual spectroscopic survey, choosing where to point a 16-fibre telescope, which target goes on
each fibre, how long to expose, and which observing program to declare, using only public data.

This is a companion to the Python example agent -- same protocol, same scoring, same packaging
rules, ported idiomatically to Node.js. See `docs/` in this project for the full protocol and
scoring spec; this README only covers what is specific to this TypeScript project.

中文说明见 [`README.zh.md`](./README.zh.md)。

## Layout

```
src/
  protocol.ts    JSON-Lines transport: message types, envelope encode/decode, stdout/stderr discipline
  skymath.ts      sky geometry: sidereal time, alt/az, sun/moon position, fibre-grid projection
  scoring.ts      scoring helpers built only from the PUBLIC scoring config (quality, program bands)
  state.ts        agent state: target catalogue, learned sky quality, per-target progress, messages
  planner.ts      decision logic: pick a pointing, fill fibres, choose exposure length and program
  memory.ts       rolling run history: progress logs (stderr) + compact context for the LLM client
  llmClient.ts    OpenAI-compatible chat client (built-in fetch; also works with Kimi/Moonshot)
  validate.ts     action validation against public limits + deterministic fallback actions
  index.ts        entry point: reads stdin, dispatches initialize/decision_request/finish
observer.project.json   platform manifest (see "Submitting" below)
.env.example             template for trying the LLM hook locally
```

Only two npm packages are used, both dev-only: `typescript` (the build step) and `@types/node`
(type declarations for `fetch`/`process`/etc., which are all built into Node.js -- no HTTP client
library is installed). There is no runtime dependency at all.

## Quick start

```bash
npm install
npm run build        # tsc -> dist/
npm start             # node dist/index.js  (reads JSON Lines from stdin, writes to stdout)
```

This package has no bundled card runner: point any protocol-compatible JSON-Lines harness (the
platform, or your own local test tool) at `node dist/index.js` with its working directory set to
this folder. The agent speaks `participant-agent-protocol-v4` on stdin/stdout and logs to stderr
only, exactly like the Python example.

Verified locally against the platform's own engine adapter on a 10,000-target, 38-night practice
card: it finishes in a few seconds of wall-clock time (the budget is 900 s), with no protocol
errors, a positive total score, and about 96% of required targets completed.

## What each module does

- **protocol.ts**: the only file that touches stdin/stdout. Defines every message shape from the
  guide (`initialize`, `decision_request`, `finish`, and our `decision_response`), and makes sure a
  response only ever contains the fields that action actually uses (an extra field ends the run).
- **skymath.ts**: a direct, dependency-free port of the engine's own formulas (local sidereal time,
  equatorial/horizontal conversion, low-precision sun/moon ephemeris, gnomonic fibre-grid
  projection) so that pointings planned here land on the fibres we expect.
- **scoring.ts**: the public parts of the scoring model (lunar factor, completion factor, program
  band thresholds) rebuilt from `initialize.payload.scoring` alone -- never from hidden truth.
- **state.ts**: owns the target catalogue and everything the agent learns at runtime: an estimated
  sky-quality scale (from its own exposure results), per-target best factor, bulletins/forecasts,
  terrain/event notices, and the once-per-run `state_resync` recovery path (Hard mode).
- **planner.ts**: for each decision, ranks visible unfinished targets (required targets that are not
  yet safe get a bonus), tries a handful of candidate pointings centred on the best targets, fills
  all 16 fibres with the best-value neighbour that lands on each one, and picks the exposure length
  and program with the best expected score per second.
- **memory.ts**: short rolling counters (hit rate, forecast notices seen) used both for stderr
  progress lines and to build small, public-data-only prompts for the LLM client.
- **llmClient.ts**: a thin OpenAI-compatible chat client using Node's built-in `fetch` (Node >= 18),
  so it works unmodified against OpenAI, the platform's own model proxy, or Kimi/Moonshot (any
  `/chat/completions`-compatible endpoint). Every call has a short timeout and the whole run has a
  small total time budget; a missing key, a slow model, or a malformed reply all fall back to `null`
  and never raise.
- **validate.ts**: checks every action (ours or the LLM's) against the public limits -- alt/az
  range, duration bounds, fibre/target duplicates, the consecutive-report cap -- before it is sent.
  Anything that fails is replaced with a safe `wait`/`finish` instead of ever reaching stdout.
- **index.ts**: wires it together. Sleeps through the day with one `until_utc` wait, closes the
  shutter for a whole-sky rain/storm bulletin, asks the LLM advisor twice each night for
  weather-avoidance/exposure-scale advice, asks it to confirm a suspected instrument fault before
  reporting (at most twice a run), and otherwise calls the planner. Every branch is wrapped so an
  internal bug never crashes the run -- it falls back to a safe `wait` instead.

### Where the LLM is used

1. **Night planning** (`llmClient.nightPlan`, called from `index.ts`'s `nightAdvice`): once per
   night, given that night's forecast notices and the current bulletin, the model suggests compass
   directions to avoid and an exposure-duration scale factor.
2. **Bulletin check-in** (`llmClient.bulletinCheckIn`, called from `index.ts`'s `nightAdvice` right
   after night planning): a second, separate call each night, given the current bulletin and the
   run's own hit rate so far, the model again suggests directions to avoid and a duration scale --
   this one reacts to what's happening right now rather than the forecast. The two answers are
   merged (avoid directions unioned, duration scale averaged) to bias pointing choice and exposure
   length for the rest of the night.
3. **Fault-report confirmation** (`llmClient.confirmReport`, called from `index.ts`'s
   `maybeReport`): when the rule-based heuristic in `state.ts` (`faultEvidence`) has already found a
   large, persistent, unexplained drop in measured sky quality across multiple nights, the model is
   asked to confirm or veto filing an instrument-fault `report` (reports are capped at twice a run;
   a wrong one costs points). This one is conditional -- it only fires when that pattern shows up.

Calls are capped (12 s per call, 300 s total budget, 100 calls max by default); a call that fails
or times out is retried a few times, and if it still doesn't come back that single decision uses
the rule-based path instead.

## LLM configuration

The client talks to any OpenAI-compatible `/chat/completions` endpoint and defaults to the Kimi
Coding Plan (see https://www.kimi.com/code/docs/en/):

- `OPENAI_BASE_URL` -- defaults to `https://api.kimi.com/coding/v1` (use
  `https://api.kimi.ai/coding/v1` for the overseas endpoint).
- `OPENAI_API_KEY` -- credential for that endpoint (`KIMI_API_KEY` also accepted).
- `OPENAI_MODEL` -- defaults to `k3`.

On the platform these are injected for every run, pointing at the platform's own model proxy and a
temporary credential. To try it locally:

```bash
cp .env.example .env     # then edit OPENAI_BASE_URL / OPENAI_API_KEY / OPENAI_MODEL
```

`.env` is not read by `node dist/index.js` automatically; export the variables in your shell, or
source `.env`, before running the harness, e.g.:

```bash
set -a && source .env && set +a && node dist/index.js < some_transcript.jsonl
```

**Never** commit a real `.env` or include one in a submission ZIP -- it is rejected if you try.

## Submitting

1. `npm run build` once locally to make sure `dist/` compiles (the platform does this for you from
   `observer.project.json`'s `build` steps; you don't need to commit `dist/`).
2. Upload this folder as a ZIP (or push it to a GitHub repository) via the Participate page.
   `observer.project.json` at the root already declares:
   - `"image": "node:20-slim"` -- Node is pulled from a standard public image; no Python is involved.
   - `"build": [["npm", "ci"], ["npm", "run", "build"]]` -- installs the two dev dependencies and
     compiles TypeScript to `dist/`.
   - `"run": ["node", "dist/index.js"]` -- runs the compiled agent.
   - `"protocol": "jsonl-v4"` -- the same JSON-Lines transport as the Python example.
3. If you add runtime dependencies, add them to `package.json`'s `dependencies` (not
   `devDependencies` -- `npm ci` installs both, but keeping the split honest documents what ships).

## Security note

This example intentionally ships **no scenario data**: no `targets.csv`/`footprint.csv`, no
`truth/` directory, no `v4_bulletins.jsonl`/`v4_forecasts.jsonl`. Everything the agent knows about a
run comes from the `initialize` message and the `new_messages` it receives live over stdin, exactly
as it will on the platform. It only ever takes the four documented actions (`observe`, `wait`,
`report`, `finish`) and only reads what the protocol hands it.

## License / Citation

Task cards, simulated data, evaluation code and this example project are licensed under
[CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/) (attribution, non-commercial);
please cite the GOSIM 2026 Agentic Observer Challenge. Your own agent code is not restricted
by this. See `LICENSE.md` for details.
