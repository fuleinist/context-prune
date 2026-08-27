# context-prune

Token compression proxy for coding agents. Sits between your agent and the LLM,
compresses tool outputs and context 40-80% with zero quality loss on structured data.

Open-source alternative to paid token-compression services — but local, fast (Rust),
and transparent.

## How it works

```
your agent ──(OpenAI-compatible HTTP)──> context-prune :8787 ──> upstream LLM API
```

The proxy intercepts request/response bodies, finds large string fields (tool
outputs, file dumps, logs), and compresses them deterministically:

- JSON blobs re-serialized compactly
- Repeated lines collapsed (`line x47` markers)
- ANSI escapes, blank-line runs, spinner noise stripped

Meaning is preserved; formatting noise is not. If anything goes wrong, the
original payload passes through unchanged. Compression never breaks a request.

## Quickstart

```bash
cargo install --path .   # or: cargo build --release
context-prune serve --upstream https://api.openai.com --port 8787

# then point your agent at it:
export OPENAI_BASE_URL=http://localhost:8787/v1
```

Check what you saved:

```bash
context-prune stats
# or: curl localhost:8787/stats
```

Try the compressor on a file:

```bash
context-prune compress big-compile-log.txt
```

## Status

Early build — see [SPEC.md](SPEC.md) for the full feature list and acceptance
criteria. Tracking cycles in the project's build journal.

## License

MIT
