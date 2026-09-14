#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="picomint-integration-bitcoind"

cleanup() {
    echo "Cleaning up..."
    # Exact process names: a `-f` substring match also hits a rustc
    # compiling one of these crates and kills a concurrent build.
    pkill -9 -x "picomint-node-daemon" 2>/dev/null || true
    pkill -9 -x "picomint-gateway-daemon" 2>/dev/null || true
    pkill -9 -x "picomint-client-daemon" 2>/dev/null || true
    pkill -9 -x "picomint-lnurl-daemon" 2>/dev/null || true
    docker stop "$CONTAINER_NAME" 2>/dev/null || true
    docker rm "$CONTAINER_NAME" 2>/dev/null || true
}

trap cleanup EXIT

echo "Building workspace..."
cargo build --workspace --release

# Clean up any leftover container from previous run
docker stop "$CONTAINER_NAME" 2>/dev/null || true
docker rm "$CONTAINER_NAME" 2>/dev/null || true

echo "Starting bitcoind in Docker..."
docker run -d \
    --name "$CONTAINER_NAME" \
    -p 18443:18443 \
    btcpayserver/bitcoin:31.0 \
    bitcoind \
    -datadir=/data \
    -regtest=1 \
    -rpcuser=bitcoin \
    -rpcpassword=bitcoin \
    -rpcallowip=0.0.0.0/0 \
    -rpcbind=0.0.0.0 \
    -rpcport=18443 \
    -fallbackfee=0.0004 \
    -txindex=0

echo "Waiting for bitcoind RPC..."
for _ in $(seq 1 60); do
    if docker exec "$CONTAINER_NAME" bitcoin-cli \
        -regtest -rpcuser=bitcoin -rpcpassword=bitcoin \
        getblockchaininfo >/dev/null 2>&1; then
        break
    fi
    sleep 0.2
done

echo "Creating wallet..."
docker exec "$CONTAINER_NAME" bitcoin-cli \
    -regtest -rpcuser=bitcoin -rpcpassword=bitcoin \
    createwallet default > /dev/null

# The mock gateway's in-process client polls at the test cadence too.
export INTEGRATION_TEST=true

echo "Running integration tests..."
RUST_LOG="${RUST_LOG:-info}" ./target/release/picomint-integration-tests
