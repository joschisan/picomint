# Pico

A minimal Picomint wallet built with Flutter and Rust.

## Features

- Lightning payments
- eCash transactions
- Multi-federation support
- Biometric authentication
- Seed phrase backup & recovery

## Setup

The bridge bindings are generated, not committed — neither `rust/src/frb_generated.rs`
nor `lib/bridge_generated.dart/` exists in a fresh checkout, and nothing compiles
until they do:

```sh
flutter pub get
flutter_rust_bridge_codegen generate
```

Re-run `generate` after changing any `pub` item in `rust/src`.

## Building

`./build-android.sh` and `./build-ios.sh` build the native library for each
platform. Both run on macOS only and are not part of CI; CI checks that the Dart
compiles against the generated bindings, and release artifacts are built locally.

## Picomint dependency

The `picomint-*` crates are git dependencies pinned by `rust/Cargo.lock`. Upstream
changes only reach this repo when that pin moves, so bump it deliberately:

```sh
cd rust && cargo update -p picomint-client -p picomint-core
```

CI runs a scheduled `upstream` job that does this against picomint `main` and fails
if the bridge no longer compiles, so drift surfaces here rather than at release time.