# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Picomint is a minimal implementation of a federated Chaumian ecash mint on Bitcoin — two binaries (federation guardian + Lightning gateway), Iroh networking, redb storage, static module set (mint, wallet, ln). No dyn modules, no migrations, no backup/recovery, no version negotiation, no legacy v1 modules. See README.md for deployment.

## Build and development

- `cargo check --workspace` — full workspace type check
- `cargo build --workspace` — build everything
- `cargo test --workspace` — run all tests
- `cargo clippy --workspace --all-targets` — lints
- `cargo fmt --all` — format
- `./test-integration.sh` — end-to-end integration test (requires Docker + bitcoind)

## Architecture

### Crates
- `picomint-core` — shared types, encoding, wire protocol, `ConsensusConfig`, and the per-module common types for `mint`/`wallet`/`ln`
- `picomint-encoding` / `picomint-derive` — `Encodable`/`Decodable` traits and derive macros
- `picomint-server-daemon` — federation guardian binary (consensus via AlephBFT); owns the concrete mint/wallet/ln server-side module code under `src/consensus/{mint,wallet,ln}/`
- `picomint-server-cli` / `picomint-server-cli-core` — admin CLI for the server daemon (HTTP-over-Unix-socket) + shared route/request types
- `picomint-gateway-daemon` — Lightning gateway binary with embedded LDK node
- `picomint-gateway-cli` / `picomint-gateway-cli-core` — admin CLI for the gateway daemon + shared route/request types
- `picomint-client` — client library; owns the concrete per-module client state machines
- `picomint-redb` — redb-based database layer
- `picomint-eventlog` — append-only client event log
- `picomint-bitcoin-rpc` — bitcoind RPC client used by the wallet module
- `picomint-recurring-daemon` — standalone recurring-payment helper daemon
- `picomint-lnurl` / `picomint-base32` / `picomint-logging` — small shared utility crates
- `picomint-integration-tests` — end-to-end integration tests (used by `test-integration.sh`)
- `picomint-startos` — StartOS packaging support

### Wire + storage
- Wire: client↔server uses the `Encodable`/`Decodable` traits from `picomint-core::encoding`
- Storage: redb only. No RocksDB. No migrations (types implement redb's `Key`/`Value` directly via macros in `picomint-redb`)
- Transport: Iroh-only (QUIC + hole-punching). No TLS/websocket/DNS announcements
- Each guardian binds exactly one iroh `Endpoint` (one secret key, one node id) for both federation p2p and the public client API; the accept loop demuxes by remote node-id (peer set → P2P path, otherwise → public API path).

### Admin CLIs
- Both CLIs are thin HTTP-over-Unix-socket clients. They POST JSON to the daemon's admin socket at `{DATA_DIR}/cli.sock` (`CLI_SOCKET_FILENAME` const in each `*-cli-core` crate). No network exposure; `docker exec` is how you reach them in a container deployment.
- Route constants live in `picomint-server-cli-core` / `picomint-gateway-cli-core`.
- Shared request/response types also live in the `*-cli-core` crates; daemon handlers live in `picomint-server-daemon/src/cli.rs` and `picomint-gateway-daemon/src/cli.rs`.

### Env vars
Env var names are unprefixed (puncture-style): `DATA_DIR`, `BITCOIN_NETWORK`, `BITCOIND_URL`, etc. No `FM_*` prefix. `*_ADDR` is the convention for listen-address vars (`P2P_ADDR`, `UI_ADDR`, `API_ADDR`, `LDK_ADDR`). Defined inline via clap `#[arg(env = "...")]`.

## Conventions

- Never `unwrap()` outside tests — use `expect("...")` with a message explaining why it can't fail.
- Prefer concrete types over dyn/trait-objects. Keep module dispatch static with typed module sets.
- No comments that explain WHAT code does — names and types already say it. Only comment non-obvious WHY.
- Prefer deleting code over preserving it — picomint is explicitly a simplification project.

## Style

- All `use` statements at the top of the file. Never inside a function body.
- Import functions/structs directly. Qualify with the containing module only when the bare name is too generic — e.g. `ln::render()` reads better than `render()`, but `Wallet::new` doesn't need `wallet::` in front of it.
- On import-name collisions, qualify inline at the use-site (e.g. `bitcoin::Network::Regtest`) rather than aliasing with `as`.
- Blank line between most statements. Exception: tight, repetitive groupings (e.g. several `let result_n = fn_n();` in a row).
- Match arms: no blank lines between branches. Mix one-liner and block-bodied arms freely.
- `///` doc comments on every `pub` item.
- Use `?` plain. Add `.context("...")` only when the underlying error is too cryptic to be useful at the boundary.
- Chain successive transformations on the same value rather than re-binding through multiple `let`s. Prefer `let x = data.iter().filter(...).map(...).collect();` over `let x = data.iter(); let x = x.filter(...); ...`.
- `thiserror` types are reserved for errors returned to the client and errors serialized via `Encodable`/`Decodable`. Use `anyhow::Result` everywhere else (orchestration, internal helpers).
