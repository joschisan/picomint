#!/usr/bin/env bash
set -e

# Set ROOT to the directory containing this script (project root)
ROOT="$(cd "$(dirname "$0")" && pwd)"

echo "🔧 Generating Rust bridge code..."
flutter_rust_bridge_codegen generate

# Check if cross is installed
if ! command -v cross &> /dev/null; then
    echo "❌ Error: 'cross' is not installed"
    echo "   Install it with: cargo install cross"
    exit 1
fi

cd $ROOT/rust

echo "🐳 Building Rust library for Android ARM64 using cross (Docker)..."
# Add 16KB page alignment for Android 15+ compatibility
RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384" cross rustc --target aarch64-linux-android --release --crate-type=cdylib

echo "📦 Copying library to jniLibs..."
mkdir -p $ROOT/android/app/src/main/jniLibs/arm64-v8a
cp $ROOT/rust/target/aarch64-linux-android/release/libpico.so $ROOT/android/app/src/main/jniLibs/arm64-v8a/

# Copy libc++_shared.so from local Android SDK/NDK
# Use the NDK version matching build.gradle.kts to ensure 16KB page alignment
NDK_VERSION="28.2.13676358"
NDK_DIR="$HOME/Library/Android/sdk/ndk/$NDK_VERSION"
if [ -f "$NDK_DIR/toolchains/llvm/prebuilt/darwin-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so" ]; then
  echo "📦 Copying libc++_shared.so from NDK $NDK_VERSION..."
  cp "$NDK_DIR/toolchains/llvm/prebuilt/darwin-x86_64/sysroot/usr/lib/aarch64-linux-android/libc++_shared.so" \
     $ROOT/android/app/src/main/jniLibs/arm64-v8a/
else
  echo "❌ Error: libc++_shared.so not found in NDK $NDK_VERSION"
  exit 1
fi

echo "✅ Build complete! You can now run: flutter run"
