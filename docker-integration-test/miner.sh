#!/bin/sh
set -eu

RPC="bitcoin-cli -regtest -rpcconnect=bitcoind -rpcuser=bitcoin -rpcpassword=bitcoin"

echo "miner: waiting for bitcoind RPC..."
until $RPC getblockchaininfo >/dev/null 2>&1; do
    sleep 0.5
done

echo "miner: ensuring wallet exists..."
$RPC createwallet miner >/dev/null 2>&1 || $RPC loadwallet miner >/dev/null 2>&1 || true

ADDR=$($RPC -rpcwallet=miner getnewaddress)
echo "miner: mining to $ADDR"

# Mine the initial 101 blocks so coinbase outputs are spendable.
HEIGHT=$($RPC getblockcount)
if [ "$HEIGHT" -lt 101 ]; then
    $RPC generatetoaddress 101 "$ADDR" >/dev/null
    echo "miner: bootstrapped to height 101"
fi

# Mine one block per second forever.
while true; do
    $RPC generatetoaddress 1 "$ADDR" >/dev/null 2>&1 || echo "miner: generate failed, retrying"
    sleep 10
done
