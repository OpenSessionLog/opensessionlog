# OpenSessionLog

OpenSessionLog is a vendor-agnostic SQLite store for AI coding agent sessions.

It ingests session data from tools like Claude Code, Codex CLI, GitHub Copilot, and OpenCode into a single normalized schema, then lets you search across all of them with full-text and semantic search.

> **Work in progress.** This repository is in early development. Build Unit 1 (Claude Code ingestion + full-text search) is currently in progress.

## Planned quickstart

```bash
# Create a vault
osl init

# Ingest Claude Code sessions
osl ingest ~/.claude/projects/

# Search across sessions
osl grep "kafka migration"

# Export a session as markdown
osl export <session-id> --format markdown > session.md
```

## License

MIT — see [LICENSE](LICENSE).

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).
