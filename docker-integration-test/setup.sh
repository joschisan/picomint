#!/usr/bin/env bash
# Drives the federation setup ceremony, joins the gateway, registers it,
# and pegs in seed liquidity. Idempotent up to a point: re-running on a
# fully wired stack will fail at `set-local-params` because guardians
# are no longer in `AwaitingLocalParams`. Tear the stack down with
# `docker compose down -v` before re-running.
set -euo pipefail

cd "$(dirname "$0")"

DC="docker compose"

# One federation with four guardians. The compose file defines services
# as `guardian-{0..3}`.
FED_NAME="Test Federation"
declare -a GUARDIANS=(0 1 2 3)
LEADER=0

echo "==> [$FED_NAME] Waiting for guardians to enter AwaitingLocalParams..."
for i in "${GUARDIANS[@]}"; do
    until $DC exec -T "guardian-$i" picomint-guardian-cli setup status 2>/dev/null \
        | grep -q AwaitingLocalParams; do
        sleep 1
    done
    echo "    guardian-$i ready"
done

echo "==> [$FED_NAME] Setting local params..."
declare -a CODES
for i in "${GUARDIANS[@]}"; do
    if [ "$i" -eq "$LEADER" ]; then
        CODES[$i]=$($DC exec -T "guardian-$i" picomint-guardian-cli setup set-local-params \
            "Guardian $i" \
            --federation-name "$FED_NAME" \
            --federation-size "${#GUARDIANS[@]}" \
            | jq -r .setup_code)
    else
        CODES[$i]=$($DC exec -T "guardian-$i" picomint-guardian-cli setup set-local-params \
            "Guardian $i" \
            | jq -r .setup_code)
    fi
done

echo "==> [$FED_NAME] Exchanging peer codes..."
for i in "${GUARDIANS[@]}"; do
    for j in "${GUARDIANS[@]}"; do
        if [ "$i" != "$j" ]; then
            $DC exec -T "guardian-$i" picomint-guardian-cli setup add-peer "${CODES[$j]}" >/dev/null
        fi
    done
done

echo "==> [$FED_NAME] Starting DKG..."
for i in "${GUARDIANS[@]}"; do
    $DC exec -T "guardian-$i" picomint-guardian-cli setup start-dkg >/dev/null
done

echo "==> [$FED_NAME] Waiting for DKG completion (invite endpoint becomes reachable)..."
INVITE=""
for _ in $(seq 1 120); do
    if INVITE=$($DC exec -T "guardian-$LEADER" picomint-guardian-cli invite 2>/dev/null \
        | jq -r .invite) && [ -n "$INVITE" ] && [ "$INVITE" != "null" ]; then
        break
    fi
    INVITE=""
    sleep 1
done

if [ -z "$INVITE" ]; then
    echo "[$FED_NAME] DKG did not complete in time" >&2
    exit 1
fi

echo "    invite: $INVITE"

echo "==> Waiting for gateway..."
until $DC exec -T gateway picomint-gateway-cli info >/dev/null 2>&1; do
    sleep 1
done

GATEWAY_PK=$($DC exec -T gateway picomint-gateway-cli info | jq -r .gateway_pk)
echo "==> Gateway iroh pk: $GATEWAY_PK"

echo "==> [$FED_NAME] Joining gateway to federation..."
$DC exec -T gateway picomint-gateway-cli federation join "$INVITE"

echo "==> [$FED_NAME] Registering gateway with federation..."
for i in "${GUARDIANS[@]}"; do
    $DC exec -T "guardian-$i" picomint-guardian-cli module ln gateway add "$GATEWAY_PK"
done

echo "==> [$FED_NAME] Funding the gateway via federation peg-in..."
FEDERATION_ID=$($DC exec -T gateway picomint-gateway-cli federation list \
    | jq -r --arg name "$FED_NAME" '.federations[] | select(.federation_name == $name) | .federation')

if [ -z "$FEDERATION_ID" ] || [ "$FEDERATION_ID" = "null" ]; then
    echo "[$FED_NAME] Could not resolve federation_id from gateway list" >&2
    exit 1
fi

DEPOSIT=$($DC exec -T gateway picomint-gateway-cli federation module wallet receive \
    --id "$FEDERATION_ID" | jq -r .address)
echo "    deposit address: $DEPOSIT"

$DC exec -T bitcoind bitcoin-cli -regtest -rpcuser=bitcoin -rpcpassword=bitcoin \
    -rpcwallet=miner sendtoaddress "$DEPOSIT" 1 >/dev/null
echo "    sent 1 BTC to gateway peg-in address"

echo "==> [$FED_NAME] Waiting for the federation to mint ecash to the gateway..."
BAL=0
for _ in $(seq 1 120); do
    BAL=$($DC exec -T gateway picomint-gateway-cli federation balance --id "$FEDERATION_ID" 2>/dev/null \
        | jq -r '.balance_msat // 0')
    if [ "${BAL:-0}" -gt 0 ]; then
        break
    fi
    sleep 2
done

if [ "${BAL:-0}" -eq 0 ]; then
    echo "[$FED_NAME] Gateway federation balance still zero — peg-in did not confirm in time" >&2
    exit 1
fi
echo "    gateway federation balance: $((BAL / 1000)) sat"

cat <<EOF

==========================================================================
Setup complete.

  $FED_NAME invite : $INVITE

  Gateway iroh pk  : $GATEWAY_PK
  LNURL daemon     : http://localhost:8091
  Guardian UIs     : http://localhost:3000..3003     (password: picomint)
  Bitcoind RPC     : http://localhost:18443          (user: bitcoin / pass: bitcoin)

The gateway holds federation ecash and can route Lightning payments
between clients. External LN routing is not configured (no second LN
node is bundled).

To reset everything: docker compose down -v
==========================================================================
EOF
