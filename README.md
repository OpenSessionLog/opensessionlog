# OpenSessionLog

OpenSessionLog is a vendor-agnostic SQLite store for AI coding agent sessions.

It ingests session data from tools like Claude Code, Codex CLI, GitHub Copilot, and OpenCode into a single normalized schema, then lets you search across all of them with full-text and semantic search.

> **Phase 2 is complete.** Phase 1 (Claude Code connector + FTS5) shipped first; Phase 2 adds semantic search, embeddings, and a file watcher daemon. See the [build plan](https://github.com/OpenSessionLog/opensessionlog/issues) for upcoming phases.

## Quickstart

```bash
# Create a vault at ~/.opensessionlog/data.sqlite (or set OSL_VAULT / --vault)
osl init

# Ingest a directory of Claude Code sessions
osl ingest ~/.claude/projects/

# Ingest a single session file
osl ingest ~/.claude/projects/my-project/session.jsonl

# Full-text search with FTS5 syntax
osl grep "kafka migration"
osl grep "kafka AND migration" --limit 10

# Semantic search (requires embedding first)
osl embed --provider ./my-embedder.sh
osl search "how does the Kafka consumer work?"

# Find sessions similar to a given one
osl similar <session-id>

# Export a session transcript to markdown
osl export <session-id> --format markdown > session.md

# Watch a directory and auto-ingest on file changes
osl watch ~/.claude/projects/
# One-shot scan (no daemon):
osl watch --once ~/.claude/projects/
```

## CLI

```text
$ osl --help
OpenSessionLog — searchable AI session vault

Usage: osl [OPTIONS] <COMMAND>

Commands:
  init     Initialize a new vault
  ingest   Ingest session files into the vault
  grep     Search messages with FTS5
  embed    Compute and store message embeddings via a user-supplied embedder script
  search   Semantic KNN search over stored embeddings
  similar  Find sessions similar to a given session by summary embedding
  watch    Watch directories and auto-ingest changed session files
  export   Export a session transcript
  help     Print this message or the help of the given subcommand(s)

Options:
      --vault <VAULT>  Vault database path (default: ~/.opensessionlog/data.sqlite; OSL_VAULT env override)
  -h, --help           Print help
  -V, --version        Print version
```

### New in Phase 2

| Command | Description |
|---|---|
| `osl embed --provider <script>` | Embed all messages using an external embedder (NDJSON over stdin/stdout). Stores results in `messages.embedding` and computes session summary embeddings. |
| `osl search <query>` | Semantic KNN search via `sqlite-vec`. Returns messages ranked by cosine distance to the embedded query. Graceful message when no embeddings exist. |
| `osl similar <session-id>` | Find sessions related to a given session by comparing their summary embeddings. |
| `osl watch [paths..]` | Daemon that monitors directories for new/changed `.jsonl` files (via inotify) and polls `.db`/`.sqlite` files at a configurable interval. Use `--once` for a single scan. |

### Embedder protocol

The `--provider` script is invoked once per `osl embed` run. It receives NDJSON on stdin (one line per message with `id` and `text` fields) and writes NDJSON to stdout: a header line with `type`, `model`, and `dimensions`, followed by one result line per input with `id` and `embedding` (float array). See [`tests/fixtures/embed/identity.py`](tests/fixtures/embed/identity.py) for a reference implementation.

### Data sources

| Source | Format | Connector |
|---|---|---|
| Claude Code | `.jsonl` event streams | `src/connector/claude.rs` |
| OpenCode | `.db` SQLite database | `src/connector/opencode.rs` |
| Codex CLI | Planned | — |
| GitHub Copilot | Planned | — |

## License

MIT — see [LICENSE](LICENSE).

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).
