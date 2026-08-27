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
compiles against the generated bindings, and the release artifacts are built
locally.