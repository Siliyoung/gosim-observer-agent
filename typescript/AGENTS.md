# AGENTS.md -- ts-agent

Guide for AI coding assistants working in this project. Humans: see README.md /
README.zh.md.

## What this is

A complete, minimal-dependency TypeScript (Node.js) agent for the GOSIM survey26
hackathon's telescope-survey challenge. It speaks `participant-agent-protocol-v4`
(one JSON object per line on stdin/stdout), runs a deterministic anchor-search
planner, and calls an LLM twice per night for forecast/bulletin advice plus
occasionally to confirm a suspected instrument fault. Read `src/index.ts` first --
it is the entry point that dispatches to everything else.

## Module map

```
src/
  protocol.ts    JSON-Lines transport: message types, envelope encode/decode
  skymath.ts      sky geometry: sidereal time, alt/az, sun/moon position, fibre grid
  scoring.ts      scoring helpers built only from the PUBLIC scoring config
  state.ts        agent state: target catalogue, learned sky quality, progress
  planner.ts      decision logic: pick a pointing, fill fibres, choose exposure/program
  memory.ts       rolling run history + compact context for the LLM client
  llmClient.ts    OpenAI-compatible chat client (built-in fetch), defaults to Kimi
  validate.ts     action validation against public limits + deterministic fallback
  index.ts        entry point: reads stdin, dispatches initialize/decision_request/finish
observer.project.json   platform manifest (see README.md "Submitting")
```

## Changing the strategy

Target ranking, fibre filling and exposure sizing live in `src/planner.ts`. The two
LLM-advised calls (forecast avoidance + bulletin check-in) are issued from
`index.ts`'s night-advice path through `llmClient.ts`; the instrument-fault
confirmation call is a third, rarer call from the same path. You can change the
ranking heuristic, add signals to `state.ts`, change what (if anything) the LLM is
asked, or replace the LLM calls entirely -- the only hard requirement enforced by
`validate.ts` is that whatever `index.ts` writes to stdout is a protocol-legal
response.

## Configuring the LLM (Kimi key / base URL / model)

Copy `.env.example` to `.env` and set:

- `OPENAI_API_KEY` (or `KIMI_API_KEY`) -- required to run the bundled planner as-is.
- `OPENAI_BASE_URL` -- defaults to the Kimi Coding Plan endpoint
  (`https://api.kimi.com/coding/v1`; use `https://api.kimi.ai/coding/v1` outside
  mainland China) when unset.
- `OPENAI_MODEL` -- defaults to `k3` when unset.

`node dist/index.js` does not read `.env` automatically -- export the variables in
your shell, or `source .env`, before running a local test harness. On the platform,
`OPENAI_BASE_URL` / `OPENAI_API_KEY` are injected automatically for every run (the
platform's own model proxy and a temporary credential); `.env` is never uploaded
and is rejected if it is included in a submission ZIP.

## Packing and uploading as a complete project

```bash
npm ci
npm run build        # tsc -> dist/
```

Then upload this folder as a ZIP (or push it to a GitHub repository) via the
Participate page. `observer.project.json` at the root already declares the build
and run steps (`npm ci && npm run build`, then `node dist/index.js`), so the
platform compiles it for you -- you do not need to commit `dist/`. Exclude
`node_modules/`, `dist/`, and `.env` from any ZIP you build by hand.

## Protocol & scoring

Full protocol reference and scoring formulas are not duplicated here -- see `docs/`
(bundled in the downloadable ZIP of this example) for the complete participant
guide in Chinese and English.
