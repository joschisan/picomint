# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Picomint is a minimal implementation of a federated Chaumian ecash mint on Bitcoin — two binaries (mint node + Lightning gateway), Iroh networking, redb storage, static module set (ecash, onchain, lightning). No dyn modules, no migrations, no backup/recovery, no version negotiation, no legacy v1 modules. See README.md for deployment.

### Naming

One vocabulary everywhere: a **mint** (the federated entity, `MintId`), run by **nodes** (consensus members, `NodeId`), bridged to Lightning by **gateways**, with **ecash** / **onchain** / **lightning** modules. Never reintroduce federation/guardian/peer/ln/gw/wallet-module. Exceptions: LDK's Lightning *peers* keep their name (channel counterparties), "LN" may refer to the Lightning Network in prose, and iroh transport identities are `PublicKey`/`iroh_pk` — "node id" always means the consensus index. "Wallet" means an actual wallet (the client's, bitcoind's, LDK's).

## Build and development

- `cargo check --workspace` — full workspace type check
- `cargo build --workspace` — build everything
- `cargo test --workspace` — run all tests
- `cargo clippy --workspace --all-targets` — lints
- `cargo fmt --all` — format
- `./test-integration.sh` — end-to-end integration test (requires Docker + bitcoind)

## Architecture

### Crates
- `picomint-core` — shared types, encoding, wire protocol, `ConsensusConfig`, and the per-module common types for `ecash`/`onchain`/`lightning`
- `picomint-encoding` / `picomint-derive` — `Encodable`/`Decodable` traits and derive macros
- `picomint-bft` — BFT atomic broadcast (DAG-based, own design — not Aleph-derived)
- `picomint-node-daemon` — mint node binary (consensus via picomint-bft); owns the concrete ecash/onchain/lightning server-side module code under `src/consensus/{ecash,onchain,lightning}/`, the bitcoind JSON-RPC client (`src/bitcoind.rs`), and the setup/dashboard web UI
- `picomint-node-cli` / `picomint-node-cli-core` — admin CLI for the node daemon (HTTP-over-Unix-socket) + shared route/request types
- `picomint-gateway-daemon` — Lightning gateway binary with embedded LDK node
- `picomint-gateway-cli` / `picomint-gateway-cli-core` — admin CLI for the gateway daemon + shared route/request types
- `picomint-client` — multi-mint client library; owns the concrete per-module client state machines and the append-only event log (`src/eventlog.rs`)
- `picomint-redb` — redb-backed typed database layer (`table!` macro; consensus-encoded keys/values)
- `picomint-rpc` — iroh RPC primitives shared by client and server (pooled connections, one request per bi stream)
- `picomint-tbs` — threshold blind signatures (BLS12-381) for ecash issuance
- `picomint-tss` — FROST threshold Schnorr (BIP 445, BIP340 output) for the mint's taproot wallet
- `picomint-tpe` — threshold point encryption (BLS12-381) for lightning contract preimages
- `picomint-fountain` — fountain-code encoder/decoder (currently unused by any other crate)
- `picomint-lnurl-daemon` — standalone LNURL proxy daemon for receiving Lightning payments
- `picomint-lnurl` / `picomint-base32` — small shared utility crates
- `picomint-integration-tests` — end-to-end integration tests (used by `test-integration.sh`)

### Wire + storage
- Wire: client↔server uses the `Encodable`/`Decodable` traits from `picomint-encoding`
- Storage: redb only. No migrations (tables are declared via the `table!` macro in `picomint-redb`; keys/values use consensus encoding). The one exception is the gateway's analytics database — a separate SQLite file (rusqlite) queried via the `query` CLI command
- Transport: Iroh-only (QUIC + hole-punching). No TLS/websocket/DNS announcements
- Each node binds exactly one iroh `Endpoint` (one secret key, one node id) for both mint p2p and the public client API; the accept loop demuxes by remote node-id (node set → P2P path, otherwise → public API path).

### Admin CLIs
- Both CLIs are thin HTTP-over-Unix-socket clients. They POST JSON to the daemon's admin socket at `{DATA_DIR}/cli.sock` (`CLI_SOCKET_FILENAME` const in each `*-cli-core` crate). No network exposure; `docker exec` is how you reach them in a container deployment.
- Route constants live in `picomint-node-cli-core` / `picomint-gateway-cli-core`.
- Shared request/response types also live in the `*-cli-core` crates; daemon handlers live in `picomint-node-daemon/src/cli.rs` and `picomint-gateway-daemon/src/cli.rs`.

### Env vars
Env var names are unprefixed (puncture-style): `DATA_DIR`, `BITCOIN_NETWORK`, `BITCOIND_URL`, etc. No `FM_*` prefix. `*_ADDR` is the convention for listen-address vars (`P2P_ADDR`, `UI_ADDR`, `API_ADDR`, `LDK_ADDR`). Defined inline via clap `#[arg(env = "...")]`.

## Conventions

- Never `unwrap()` outside tests — use `expect("...")` with a message explaining why it can't fail.
- Prefer concrete types over dyn/trait-objects. Keep module dispatch static with typed module sets.
- No comments that explain WHAT code does — names and types already say it. Only comment non-obvious WHY.
- Prefer deleting code over preserving it — picomint is explicitly a simplification project.

## Style

- All `use` statements at the top of the file. Never inside a function body.
- Import functions/structs directly. Qualify with the containing module only when the bare name is too generic — e.g. `lightning::render()` reads better than `render()`, but `Client::new` doesn't need `client::` in front of it.
- On import-name collisions, qualify inline at the use-site (e.g. `bitcoin::Network::Regtest`) rather than aliasing with `as`.
- Blank line between most statements. Exception: tight, repetitive groupings (e.g. several `let result_n = fn_n();` in a row).
- Match arms: no blank lines between branches. Mix one-liner and block-bodied arms freely.
- `///` doc comments on every `pub` item.
- Use `?` plain. Add `.context("...")` only when the underlying error is too cryptic to be useful at the boundary.
- Chain successive transformations on the same value rather than re-binding through multiple `let`s. Prefer `let x = data.iter().filter(...).map(...).collect();` over `let x = data.iter(); let x = x.filter(...); ...`.
- `thiserror` types are reserved for errors returned to the client and errors serialized via `Encodable`/`Decodable`. Use `anyhow::Result` everywhere else (orchestration, internal helpers).
