# Build journal — benchmarks

## Cycle 5: release profile bench (2026-08-28)

`cargo build --release` (current profile: `lto = "thin"`), Windows x64.
Corpus generated in `target/` (gitignored): a 3.27 MB pretty-printed JSON
message log and a 1.52 MB text log with runs of 100 consecutive identical
lines. New `context-prune bench <file>` subcommand measures throughput over
the full parse → compress → serialize path.

| corpus | mode | savings | debug | release | speedup |
|---|---|---|---|---|---|
| 3.27 MB JSON log | json | 3.2% | 19.8 MB/s (157.7 ms/iter) | 377.9 MB/s (8.3 ms/iter) | ~19x |
| 1.52 MB repeated-line log | text | 98.6% | 14.3 MB/s (101.9 ms/iter) | 191.3 MB/s (7.6 ms/iter) | ~13x |

Notes:

- Release throughput (191–378 MB/s) is far above any proxy bottleneck —
  network and upstream LLM latency dominate, compression is not a concern.
- Debug text mode only collapses *consecutive* identical lines; interleaved
  duplicates pass through (savings 0% on the earlier interleaved corpus).
  Expected behavior, not a bug.
- JSON-mode savings on synthetic chat content are modest (3.2%); real wins
  come from tool output / log blobs, covered by the e2e suite.

## Cycle 4 (2026-08-28)

SSE streaming passthrough e2e coverage — mock SSE endpoint, event-stream
content-type + payload preservation checks. Suite 10/10 green (0b86ae3).

## Cycle 3

E2e suite (8/8) + upstream header passthrough fix (5fae664).

## Cycle 2

Rust core: compression engine, axum proxy, SQLite stats, CLI (806dbd2).
