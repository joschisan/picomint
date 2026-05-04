#!/usr/bin/env bash
# Drives the federation setup ceremony, joins the gateway, and registers
# it with the federation. Idempotent up to a point: re-running on a fully
# wired stack will fail at `set-local-params` because guardians are no
# longer in `AwaitingLocalParams`. Tear the stack down with
# `docker compose down -v` before re-running.
set -euo pipefail

cd "$(dirname "$0")"

DC="docker compose"

GUARDIANS=(0 1 2 3)
LEADER=0

echo "==> Waiting for guardians to enter AwaitingLocalParams..."
for i in "${GUARDIANS[@]}"; do
    until $DC exec -T "guardian-$i" picomint-server-cli setup status 2>/dev/null \
        | grep -q AwaitingLocalParams; do
        sleep 1
    done
    echo "    guardian-$i ready"
done

echo "==> Setting local params..."
declare -a CODES
for i in "${GUARDIANS[@]}"; do
    if [ "$i" -eq "$LEADER" ]; then
        CODES[$i]=$($DC exec -T "guardian-$i" picomint-server-cli setup set-local-params \
            "Guardian $i" \
            --federation-name "Picomint Test Federation" \
            --federation-size "${#GUARDIANS[@]}" \
            | jq -r .setup_code)
    else
        CODES[$i]=$($DC exec -T "guardian-$i" picomint-server-cli setup set-local-params \
            "Guardian $i" \
            | jq -r .setup_code)
    fi
done

echo "==> Exchanging peer codes..."
for i in "${GUARDIANS[@]}"; do
    for j in "${GUARDIANS[@]}"; do
        if [ "$i" != "$j" ]; then
            $DC exec -T "guardian-$i" picomint-server-cli setup add-peer "${CODES[$j]}" >/dev/null
        fi
    done
done

echo "==> Starting DKG..."
for i in "${GUARDIANS[@]}"; do
    $DC exec -T "guardian-$i" picomint-server-cli setup start-dkg >/dev/null
done

echo "==> Waiting for DKG completion (invite endpoint becomes reachable)..."
INVITE=""
for _ in $(seq 1 120); do
    if INVITE=$($DC exec -T "guardian-$LEADER" picomint-server-cli invite 2>/dev/null \
        | jq -r .invite_code) && [ -n "$INVITE" ] && [ "$INVITE" != "null" ]; then
        break
    fi
    INVITE=""
    sleep 1
done

if [ -z "$INVITE" ]; then
    echo "DKG did not complete in time" >&2
    exit 1
fi

echo "    invite: $INVITE"

echo "==> Waiting for gateway..."
until $DC exec -T gateway picomint-gateway-cli info >/dev/null 2>&1; do
    sleep 1
done

echo "==> Joining gateway to federation..."
$DC exec -T gateway picomint-gateway-cli federation join "$INVITE"

echo "==> Registering gateway with federation..."
GATEWAY_URL="${GATEWAY_URL:-http://$(curl -fsS --max-time 5 https://api.ipify.org):8090}"
echo "    gateway URL: $GATEWAY_URL"
for i in "${GUARDIANS[@]}"; do
    $DC exec -T "guardian-$i" picomint-server-cli module ln gateway add "$GATEWAY_URL"
done

echo "==> Funding the gateway via federation peg-in..."
GW_DEPOSIT=$($DC exec -T gateway picomint-gateway-cli federation module wallet receive | jq -r .address)
echo "    deposit address: $GW_DEPOSIT"

$DC exec -T bitcoind bitcoin-cli -regtest -rpcuser=bitcoin -rpcpassword=bitcoin \
    -rpcwallet=miner sendtoaddress "$GW_DEPOSIT" 1 >/dev/null
echo "    sent 1 BTC to gateway peg-in address"

echo "==> Waiting for the federation to mint ecash to the gateway..."
for _ in $(seq 1 120); do
    BAL=$($DC exec -T gateway picomint-gateway-cli federation balance 2>/dev/null \
        | jq -r '.balance_msat // 0')
    if [ "${BAL:-0}" -gt 0 ]; then
        break
    fi
    sleep 2
done

if [ "${BAL:-0}" -eq 0 ]; then
    echo "Gateway federation balance still zero — peg-in did not confirm in time" >&2
    exit 1
fi
echo "    gateway federation balance: $((BAL / 1000)) sats"

cat <<EOF

==========================================================================
Setup complete.

  Federation invite : $INVITE
  Gateway URL       : $GATEWAY_URL   (registered with federation)
  Recurring daemon  : http://localhost:8091
  Guardian UIs      : http://localhost:3000..3003   (password: picomint)
  Bitcoind RPC      : http://localhost:18443        (user: bitcoin / pass: bitcoin)

The gateway holds federation ecash and can route Lightning payments
between clients of this federation. External LN routing is not
configured (no second LN node is bundled).

To reset everything: docker compose down -v
==========================================================================
EOF
