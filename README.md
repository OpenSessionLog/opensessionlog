# OpenSessionLog

OpenSessionLog is a vendor-agnostic SQLite store for AI coding agent sessions.

It ingests session data from tools like Claude Code, Codex CLI, GitHub Copilot, and OpenCode into a single normalized schema, then lets you search across all of them with full-text and semantic search.

> **Phase 1 is complete.** This release supports Claude Code `.jsonl` session files only. Other connectors, file watching, and semantic search are planned for later units.

## Quickstart

```bash
# Create a vault at ~/.opensessionlog/data.sqlite (or set OSL_VAULT / --vault)
osl init

# Ingest a directory of Claude Code sessions
osl ingest ~/.claude/projects/

# Ingest a single session file
osl ingest ~/.claude/projects/my-project/session.jsonl

# Search across sessions with FTS5 syntax
osl grep "kafka migration"
osl grep "kafka AND migration" --limit 10

# Export a session transcript to markdown
osl export <session-id> --format markdown > session.md
```

## CLI help

```text
$ osl --help
OpenSessionLog — searchable AI session vault

Usage: osl [OPTIONS] <COMMAND>

Commands:
  init     Initialize a new vault
  ingest   Ingest session files into the vault
  grep     Search messages with FTS5
  export   Export a session transcript
  help     Print this message or the help of the given subcommand(s)

Options:
      --vault <VAULT>  Vault database path (default: ~/.opensessionlog/data.sqlite; OSL_VAULT env override)
  -h, --help           Print help
  -V, --version        Print version
```

## License

MIT — see [LICENSE](LICENSE).

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).
