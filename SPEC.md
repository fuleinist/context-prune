# context-prune — SPEC

Token compression proxy for coding agents. Sits between your agent and the LLM,
compresses tool outputs and conversation context 40-80% with zero quality loss on
structured data. Open-source Headroom alternative.

## Problem

Every agent session pays for tokens twice: once for context the model needs, once
for context it doesn't. Tool outputs are the worst offender — `ls -R`, grep dumps,
JSON blobs, log files, build output. Most of that is whitespace, repeated paths,
boilerplate, and structure the model could reconstruct from a fraction of the bytes.

## Design Principles

1. **Lossless for meaning, lossy for noise.** Compression must never drop
   information a model might need to answer correctly. It drops formatting,
   dedupes, summarizes structure — never facts.
2. **Zero-config passthrough.** Point `OPENAI_BASE_URL` at the proxy, everything
   works. No config = pure passthrough, same behavior as upstream.
3. **Fast.** Compression happens in-process in Rust. Overhead < 50ms p99 on 1MB payloads.
4. **Observable.** Every request logs bytes-in, bytes-out, compression ratio.
5. **Local-first.** SQLite for stats/history. No external service required.

## Architecture

```
agent (Claude Code, Codex, curl)
   │  OpenAI-compatible HTTP
   ▼
context-prune proxy (axum, port 8787)
   │  1. intercept request/response
   │  2. detect compressible content (tool results, large strings)
   │  3. compress: structural / dedup / noise-stripping
   │  4. forward
   ▼
upstream LLM API (OpenAI, Anthropic, local)
```

## Features (v1)

### F1 — OpenAI-compatible reverse proxy (core)
- Accept any request at `/v1/*`, forward to configured upstream.
- Preserve headers, streaming (SSE passthrough), auth.
- **Acceptance:** `curl localhost:8787/v1/models` returns upstream's model list.
  A real chat completion with streaming works end-to-end.

### F2 — Tool-output compression
- On response bodies (and request bodies containing prior tool results), find
  string fields above a size threshold (default 2KB) and compress them:
  - **JSON mode:** re-serialize compactly, collapse whitespace.
  - **Lines mode:** collapse duplicate/repeated lines (`x47` markers), truncate
    long runs of identical prefixes (common file paths).
  - **Noise mode:** strip ANSI escapes, strip blank-line runs, strip
    progress-bar/spinner lines.
- Compression is conservative by default; each transform is individually
  reversible-in-spirit (structure preserved) and can be toggled.
- **Acceptance:** A 100KB `ls -R` style payload compresses ≥ 40% with no file
  names lost (verified by round-trip extraction of path tokens).

### F3 — Compression stats endpoint
- `GET /stats` returns JSON: requests seen, bytes in/out, average ratio,
  top endpoints.
- SQLite persistence across restarts.
- **Acceptance:** After 5 proxied requests, `/stats` shows correct totals.

### F4 — CLI
- `context-prune serve` — run the proxy (flags: `--port`, `--upstream`,
  `--min-size`, `--db`).
- `context-prune stats` — print stats from the DB.
- `context-prune compress <file>` — one-shot compression demo, prints ratio.
- **Acceptance:** `context-prune --help` lists all three subcommands; each works.

### F5 — Safety
- Compression failures NEVER break the request — on any error, forward
  the original payload unchanged.
- Never compress inside `Authorization` headers or tool-call arguments that
  look like code the model will execute (heuristics + config).
- **Acceptance:** Malformed JSON input passes through unchanged; proxy never
  returns 5xx due to compression bugs (test-covered).

## Stretch Goals (v2)

- [x] Tree-sitter-aware code summarization (keep signatures, drop bodies).
  Done cycle 7: `context-prune skeleton <file>` (feature `skeleton`).
- [x] Per-model compression profiles. Done cycle 8: `--profile default|
  conservative|aggressive` on `serve`/`compress`; small-context models
  (haiku/flash/mini/nano) auto-route to `aggressive` via the request's
  `model` field. `--min-size` overrides the profile's floor.
- [ ] Local code-graph context builder (like codegraph).
- [ ] Cache of compressed blobs keyed by content hash.

## Tech Stack

- Rust 2024, axum + hyper (proxy), serde_json (parsing), rusqlite (stats), clap (CLI).
- No tokio-unstable features; MSRV = current stable.

## Acceptance Criteria Summary

1. Proxy passes OpenAI-compatible requests through unchanged when compression disabled.
2. Streaming SSE responses work.
3. Tool-output compression achieves ≥ 40% on structured fixtures.
4. Compression never loses path/fact tokens in tests.
5. `/stats` endpoint + SQLite persistence work.
6. CLI has serve/stats/compress subcommands.
7. `cargo test` green; `cargo clippy` clean.

## Out of Scope (v1)

- Anthropic-native API translation (OpenAI-compat only; upstream is OpenAI-shaped).
- Any cloud component.
- Semantic/LLM-based summarization (v1 is deterministic only).
