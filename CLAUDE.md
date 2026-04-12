# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Headjack is a Rust library (not a binary) providing a Matrix bot framework. It wraps `matrix-rust-sdk` (0.7) to simplify writing Matrix bots, handling authentication, session persistence, event handling, command dispatching, and room management.

## Build & Development Commands

All commands assume the Nix dev shell is active (via direnv/`use flake`).

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

- **`src/lib.rs`** - Core framework. `Bot` struct holds config, sync token, and Matrix `Client`. `BotConfig` configures login credentials, allow-lists (regex-based sender filtering), command prefix, state directory, and room size limits. Event handlers for messages, commands, and room invites are registered via `register_text_command()`, `register_text_handler()`, and `join_rooms_callback()`. Session persistence uses an encrypted SQLite database.

- **`src/utils.rs`** - Matrix room tag utilities. `Tags<'a>` provides type-safe room tagging with namespace support (e.g., `tld.domain.tag`) and key-value tag helpers. Auto-syncs on drop.

### Key Design Constraints

- **Global state via `lazy_static!`**: Required because `matrix-sdk` event handlers require `'static` lifetimes. Command handlers and text handlers are stored in global `Vec`s.
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
