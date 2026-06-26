# Embedder Providers for OpenSessionLog

An embedder provider is an executable script that `osl embed --provider <script>` invokes as a subprocess. The vault never calls an embedding API directly — the user brings their own embedder.

## Protocol

The script is invoked once per `osl embed` run and communicates over stdin/stdout via NDJSON (newline-delimited JSON).

### Input (stdin)

One JSON line per message to embed:

```json
{"id": "<message-uuid>", "text": "<role>\\n<message-content-or-thinking>"}
```

osl closes stdin after writing all lines.

### Output (stdout)

**First line** — model header:

```json
{"type": "model", "model": "<model-name>", "dimensions": 128}
```

**Subsequent lines** — one per input message, in the same order:

```json
{"id": "<message-uuid>", "embedding": [0.012, -0.034, ...]}
```

The header line must always be emitted, even if there are zero input lines.

### Exit code

Exit 0 on success, non-zero on failure (osl forwards stderr for diagnostics).

---

## Examples

| Provider | Description | Requirements |
|---|---|---|
| [`openai.sh`](openai.sh) | OpenAI `text-embedding-3-small` API | `curl`, `jq`, `OPENAI_API_KEY` env var |
| [`llamafile.sh`](llamafile.sh) | Local llamafile server embedding endpoint | A running [llamafile](https://github.com/Mozilla-Ocho/llamafile) server |
| [`sentence-transformers.py`](sentence-transformers.py) | Local Python via sentence-transformers | `sentence-transformers`, `torch` |
| [`identity.py`](../../tests/fixtures/embed/identity.py) | Deterministic test fixture (8-dim, hashing only) | Python 3 stdlib |

## Creating your own

Any executable works. The protocol is intentionally minimal — wrap any embedding API or library in under 50 lines. Key rules:

1. Write the header line **before** reading stdin (the model name and dimension must be declared upfront)
2. Flush stdout after the header so osl knows it's ready (`flush=True` in Python, `fflush(stdout)` in C, etc.)
3. Output results in the same order as inputs
4. For zero input lines, still emit the header and exit 0 (no-op)
5. Exit non-zero and explain the error on stderr on failure
