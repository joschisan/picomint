#!/usr/bin/env bash
set -euo pipefail

CONTAINER_NAME="picomint-integration-bitcoind"

cleanup() {
    echo "Cleaning up..."
    # Bare `-x` matches on comm — kernel-truncated to 15 chars
    # (TASK_COMM_LEN), so `pkill -x picomint-node-daemon` (20 chars)
    # matches nothing and the daemons leak across iterations. `-f -x`
    # matches the full argv exactly, and each daemon is spawned with
    # only this path as its argv[0], so a cargo/rustc invocation that
    # merely mentions the substring won't collide.
    pkill -9 -f -x "target/release/picomint-node-daemon" 2>/dev/null || true
    pkill -9 -f -x "target/release/picomint-gateway-daemon" 2>/dev/null || true
    pkill -9 -f -x "target/release/picomint-client-daemon" 2>/dev/null || true
    pkill -9 -f -x "target/release/picomint-lnurl-daemon" 2>/dev/null || true
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
