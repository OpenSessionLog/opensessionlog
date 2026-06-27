# OpenSessionLog

OpenSessionLog is a vendor-agnostic SQLite store for AI coding agent sessions.

It ingests session data from tools like Claude Code, Codex CLI, GitHub Copilot, and OpenCode into a single normalized schema, then lets you search across all of them with full-text and semantic search.

> **Phase 3 is complete.** Phase 1 (Claude Code connector + FTS5) shipped first; Phase 2 added semantic search, embeddings, and a file watcher daemon; Phase 3 adds usage aggregation and reporting. See the [build plan](https://github.com/OpenSessionLog/opensessionlog/issues) for upcoming phases.

## Quickstart

```bash
# Guided first-run (recommended for new users)
osl setup                           # init + discover + ingest all sources, skip embedding
osl setup --recency 120            # only sessions from the last 120 days
osl setup --provider ./my-embedder.sh   # also run the embed step
osl setup --ingest-recency 365 --embed-recency 120 --provider ./my-embedder.sh
                                     # wide FTS history, narrow embed window (cost control)

# The commands below show the individual steps `setup` automates.
# Create a vault at ~/.opensessionlog/data.sqlite (or set OSL_VAULT / --vault)
osl init

# Ingest a directory of Claude Code sessions
osl ingest ~/.claude/projects/

# Ingest a single session file
osl ingest ~/.claude/projects/my-project/session.jsonl

# Ingest Hermes Agent session database (auto-detected by schema)
osl ingest ~/.hermes/state.db

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

# Generate a usage report for the last 30 days
osl report --period last-30-days

# Generate a report for a specific date range in JSON
osl report --from 2026-06-01 --to 2026-06-15 --format json

# Persist a report (closed periods served from cache on re-run)
osl report --period monthly --save
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
  report   Aggregate usage into a period report (markdown or JSON)
  setup    Guided first-run: init, discover, ingest, and optionally embed
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

### New in Phase 3

| Command | Description |
|---|---|
| `osl report [--period <kind>] [--from <date> --to <date>]` | Aggregate session usage into a period report. Supports named periods (`daily`, `weekly`, `monthly`, `last-30-days`) or custom date ranges. Outputs markdown (default) or JSON. |
| `osl report --project <slug>` | Filter the report to a single project. |
| `osl report --source <name>` | Filter the report to a single source (e.g. `claude`, `codex`, `opencode`). |
| `osl report --save` | Persist the report to the `reports` table. Closed periods (ended before today) are served from cache on subsequent runs; open periods are re-aggregated. |

**Example output:**
```text
$ osl report --period weekly
# Usage Report — global

- **Period:** weekly (2026-06-20 → 2026-06-26)
- **Generated:** 2026-06-26T21:42:14Z
- **Source:** fresh

## Totals
- Sessions: 42
- Tokens: in=1,500,000 out=800,000 cache_r=600,000 cache_w=200,000 total=3,100,000
- Estimated cost: no data
- Messages: 1,200
- Tool calls: 350
- Errors: 5
- Unique models: 2
- Avg session duration: 1,842s

## Messages by role
- user: 400
- assistant: 800

## Top tools
1. Bash — 200
2. Edit — 100
3. Read — 50

## Top projects
1. opensessionlog — 15 sessions, 500,000 tokens

## Sources
- claude — 30 sessions, 2,200,000 tokens
- opencode — 12 sessions, 900,000 tokens

## Daily breakdown
| date       | sessions | messages | tools | tokens |
|------------|----------|----------|-------|--------|
| 2026-06-20 | 8        | 200      | 60    | 500,000 |
| 2026-06-21 | 5        | 150      | 40    | 400,000 |
```

### New in Phase 4

| Command | Description |
|---|---|
| `osl setup` | Guided first-run: initialize a vault, discover known source directories, ingest all sessions, and optionally run embeddings. |
| `osl setup --recency 120` | Ingest and embed only sessions/messages from the last 120 days. |
| `osl setup --ingest-recency 365 --embed-recency 120 --provider ./embedder.sh` | Ingest a year of history into FTS, but only embed the last 120 days. |
| `osl setup --force --provider ./embedder.sh` | Re-embed all in-scope messages, even if an embedding already exists. |

`osl setup` discovers sessions from Claude Code (`~/.claude/projects/`), Codex CLI (`~/.codex/sessions/`), OpenCode (`~/.config/opencode/opencode.db`), and Hermes Agent (`~/.hermes/state.db` or XDG equivalents). Pi and GitHub Copilot are planned but not yet supported (Copilot sessions are currently handled inside the Codex connector).

Flag rules:
- `--recency` and `--since` apply the same window to both ingest and embed.
- `--ingest-recency` and `--embed-recency` split the windows: if only `--ingest-recency` is set, embed uses the same window; if only `--embed-recency` is set, ingest defaults to **all** (no accidental data loss).
- The four flag families are mutually exclusive: do not mix `--recency`/`--since` with `--ingest-recency`/`--embed-recency`.
- When stdin is not a TTY, `osl setup` runs non-interactively (equivalent to `--yes`).

### Recency-filtered ingestion and embedding (Issue #13)

`osl ingest` and `osl embed` accept independent `--recency <days>` and `--since <date>`
flags so you can ingest a wide history but only pay for embeddings on recent work.

| Flag | Applies to | Behavior |
|------|-----------|----------|
| `osl ingest --recency 365` | `ingest` | Ingest sessions whose JSONL file `mtime` (or SQLite `started_at`/`time_created`) falls in the last 365 days |
| `osl ingest --since 2026-03-01` | `ingest` | Ingest sessions on or after `2026-03-01` |
| `osl embed --recency 120` | `embed` | Embed messages from the last 120 days (vault `messages.created_at`) |
| `osl embed --since 2026-06-01` | `embed` | Embed messages on or after `2026-06-01` |
| `osl embed --force` | `embed` | Re-embed every matching message, even if an embedding already exists |

Rules:
- `--recency` and `--since` are mutually exclusive within a single subcommand.
- `osl ingest --recency 365` then `osl embed --recency 120` is a first-class decoupled workflow: ingest a year of history, but only embed the most recent 120 days.
- `osl embed` without `--force` is incremental: it only embeds messages with `embedding IS NULL`. Re-running `osl embed --recency 30` is safe and will fill in newly-ingested messages in that window.
- `--since` accepts strict `YYYY-MM-DD` (UTC, inclusive lower bound).
- With no flags, `osl ingest` ingests everything and `osl embed` embeds every NULL-embedding message — unchanged from before.

### Embedder providers

The `--provider` script is invoked once per `osl embed` run. It receives NDJSON on stdin (one line per message with `id` and `text` fields) and writes NDJSON to stdout: a header line with `type`, `model`, and `dimensions`, followed by one result line per input with `id` and `embedding` (float array).

**Ready-to-use examples** ([`examples/embed-providers/`](examples/embed-providers/)):

| Provider | Description | Requirements |
|---|---|---|
| [`openai.sh`](examples/embed-providers/openai.sh) | OpenAI `text-embedding-3-small` via API | `curl`, `jq`, `OPENAI_API_KEY` |
| [`llamafile.sh`](examples/embed-providers/llamafile.sh) | Local llamafile server embedding endpoint | Running [llamafile](https://github.com/Mozilla-Ocho/llamafile) server |
| [`sentence-transformers.py`](examples/embed-providers/sentence-transformers.py) | Local Python via sentence-transformers | `sentence-transformers`, `torch` |
| [`identity.py`](tests/fixtures/embed/identity.py) | Deterministic test fixture (8-dim, hashing) | Python 3 stdlib |

The full protocol spec and guidance for writing custom providers is in [`examples/embed-providers/README.md`](examples/embed-providers/README.md).

### Data sources

| Source | Format | Connector |
|---|---|---|---|
| Claude Code | `.jsonl` event streams | `src/connector/claude.rs` |
| OpenCode | `.db` SQLite database | `src/connector/opencode.rs` |
| Hermes Agent | `.db` SQLite database (`state.db`) | `src/connector/hermes.rs` |
| Codex CLI | `.jsonl` event streams (auto-detected from Claude format) | `src/connector/codex.rs` |
| GitHub Copilot | Planned | — |

## License

MIT — see [LICENSE](LICENSE).

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).
