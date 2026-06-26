# Contributing to OpenSessionLog

Thank you for your interest in contributing!

## Building

You need a stable Rust toolchain. We pin it in `rust-toolchain.toml`.

```bash
cargo build
cargo test
```

## Code quality

We require all PRs to pass:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Adding a connector

Connectors live in `src/connector/<name>.rs` and implement the `Connector` trait. Each connector is responsible for:

1. Discovering session files in a directory
2. Parsing a session file into `NormalizedSession`
3. Deriving stable UUIDs via `ids::session_id` and `ids::message_id`

See `src/connector/claude.rs` for the reference implementation.

## Dependency updates

`Cargo.lock` is committed for reproducible CI builds. Please make dependency bumps intentional and explain them in the PR description.
