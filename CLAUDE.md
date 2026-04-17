# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Headjack is a Rust library (not a binary) providing a Matrix bot framework. It wraps `matrix-rust-sdk` (0.16) to simplify writing Matrix bots, handling authentication, session persistence, event handling, command dispatching, and room management.

## Build & Development Commands

All commands assume the Nix dev shell is active (via direnv/`use flake`).

**Note:** Rust is pinned to 1.93.0 in the Nix flake due to a rustc 1.94+ regression (rust-lang/rust#152942) that causes matrix-sdk 0.16 to hit query depth limits. Do not unpin until matrix-org/matrix-rust-sdk#6254 is resolved.

| Task              | Command                                            |
| ----------------- | -------------------------------------------------- |
| Build             | `cargo build` or `just build`                      |
| Run tests         | `cargo nextest run` or `just test`                 |
| Run a single test | `cargo nextest run <test_name>`                    |
| Clippy            | `cargo clippy`                                     |
| Format            | `just fmt` (treefmt: rustfmt, alejandra, prettier) |
| Lint              | `just lint`                                        |
| Full local CI     | `just ci`                                          |
| Nix flake check   | `just nix check`                                   |

CI enforces clippy with all warnings denied (`-D warnings`), so fix all clippy warnings before committing.

## Architecture

This is a single-crate library with two source files:

- **`src/lib.rs`** - Core framework. `BotConfig` holds login credentials, allow-lists (regex-based sender filtering), command prefix, state directory, and room size limits. `BotConfig::login()` connects to Matrix and returns a `Bot` — which always holds a valid `Client` (no `Option`). Event handlers for messages, commands, room invites, and arbitrary matrix-sdk events are registered on `Bot`. Session persistence uses JSON files on disk.

- **`src/utils.rs`** - Matrix room tag utilities. `Tags<'a>` provides type-safe room tagging with namespace support (e.g., `tld.domain.tag`) and key-value tag helpers. Auto-syncs on drop.

### Key Types

- `BotConfig` — Pre-login configuration. Call `.login()` to connect and get a `Bot`.
- `Bot` — Connected bot with a live `Client`. All registration and sync methods live here.
- `RetryConfig` — Configuration for `Bot::run_with_retry()` (delay, max retries).

### Key Design Patterns

- **`BotConfig` → `Bot`**: Login consumes config and returns a connected bot. No `Option<Client>`, no panicking getters.
- **Local state**: Help text stored in `Arc<Mutex<State>>` on `Bot`, not in globals. Shared with handler closures via `Arc::clone`.
- **Generic event handlers**: `bot.add_event_handler()` delegates to matrix-sdk for any event type. Text-specific helpers (`register_text_command`, `register_text_handler`) add allow-list filtering on top.
- **Sync lifecycle**: `sync_once()` for single sync, `run()` for sync loop (returns on error), `run_with_retry()` for sync loop with configurable retry.
- **Fully async**: Built on Tokio with `rt-multi-thread`.
- **Exponential backoff**: Auto-join retries with a cap at 3600s.
- **Lazy-loading**: Matrix room members are lazy-loaded during sync for efficiency.

## CI/CD

GitHub Actions runs on push/PR to main:

- `ci.yml`: nix-fast-build based — lint, test, build
- `security-audit.yml`: daily cargo-deny, auto-creates GitHub issues
- `release-plz.yml`: automated versioning and crates.io publishing

## Release Process

Releases are automated via `release-plz`. Conventional commit messages drive changelog generation (via `git-cliff` / `cliff.toml`). Pushing to main triggers release PR creation and crates.io publishing.
