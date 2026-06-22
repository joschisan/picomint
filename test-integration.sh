#!/usr/bin/env bash
set -euo pipefail

# Pass --keep-alive to leave the federation running (skipping the test flows)
# for hands-on / phone testing instead of running the suite and exiting.
KEEP_ALIVE=
if [[ "${1:-}" == "--keep-alive" ]]; then
    KEEP_ALIVE=1
fi

CONTAINER_NAME="picomint-integration-bitcoind"

cleanup() {
    echo "Cleaning up..."
    pkill -9 -f "picomint-guardian-daemon" 2>/dev/null || true
    pkill -9 -f "picomint-gateway-daemon" 2>/dev/null || true
    pkill -9 -f "picomint-lnurl-daemon" 2>/dev/null || true
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

if [[ -n "$KEEP_ALIVE" ]]; then
    echo "Bringing up federation (stays up until Ctrl-C)..."
    KEEP_ALIVE=1 RUST_LOG="${RUST_LOG:-info}" ./target/release/picomint-integration-tests
else
    echo "Running integration tests..."
    RUST_LOG="${RUST_LOG:-info}" ./target/release/picomint-integration-tests
fi
