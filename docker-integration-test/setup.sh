#!/usr/bin/env bash
# Drives the federation setup ceremony for both bundled federations,
# joins the gateway to each, registers it, and pegs in seed liquidity.
# Idempotent up to a point: re-running on a fully wired stack will fail
# at `set-local-params` because guardians are no longer in
# `AwaitingLocalParams`. Tear the stack down with
# `docker compose down -v` before re-running.
set -euo pipefail

cd "$(dirname "$0")"

DC="docker compose"

GATEWAY_URL="${GATEWAY_URL:-http://$(curl -fsS --max-time 5 https://api.ipify.org):8090}"
echo "==> Gateway URL: $GATEWAY_URL"

# Two federations, each with four guardians of its own. The compose file
# defines services as `${prefix}-guardian-{0..3}`. Federation names are
# user-visible, prefix is the docker compose service-name prefix.
declare -a FED_NAMES=("Test Federation I" "Test Federation II")
declare -a FED_PREFIXES=("fed1" "fed2")
declare -a GUARDIANS=(0 1 2 3)
LEADER=0

declare -a INVITES

# Drive DKG for one federation. Leaves the federation live and prints
# the invite code on stdout (caller captures it). All progress noise
# goes to stderr so the captured value is just the invite.
setup_federation() {
    local fed_name="$1"
    local prefix="$2"

    echo "==> [$fed_name] Waiting for guardians to enter AwaitingLocalParams..." >&2
    for i in "${GUARDIANS[@]}"; do
        until $DC exec -T "${prefix}-guardian-$i" picomint-server-cli setup status 2>/dev/null \
            | grep -q AwaitingLocalParams; do
            sleep 1
        done
        echo "    ${prefix}-guardian-$i ready" >&2
    done

    echo "==> [$fed_name] Setting local params..." >&2
    declare -a CODES
    for i in "${GUARDIANS[@]}"; do
        if [ "$i" -eq "$LEADER" ]; then
            CODES[$i]=$($DC exec -T "${prefix}-guardian-$i" picomint-server-cli setup set-local-params \
                "Guardian $i" \
                --federation-name "$fed_name" \
                --federation-size "${#GUARDIANS[@]}" \
                | jq -r .setup_code)
        else
            CODES[$i]=$($DC exec -T "${prefix}-guardian-$i" picomint-server-cli setup set-local-params \
                "Guardian $i" \
                | jq -r .setup_code)
        fi
    done

    echo "==> [$fed_name] Exchanging peer codes..." >&2
    for i in "${GUARDIANS[@]}"; do
        for j in "${GUARDIANS[@]}"; do
            if [ "$i" != "$j" ]; then
                $DC exec -T "${prefix}-guardian-$i" picomint-server-cli setup add-peer "${CODES[$j]}" >/dev/null
            fi
        done
    done

    echo "==> [$fed_name] Starting DKG..." >&2
    for i in "${GUARDIANS[@]}"; do
        $DC exec -T "${prefix}-guardian-$i" picomint-server-cli setup start-dkg >/dev/null
    done

    echo "==> [$fed_name] Waiting for DKG completion (invite endpoint becomes reachable)..." >&2
    local invite=""
    for _ in $(seq 1 120); do
        if invite=$($DC exec -T "${prefix}-guardian-$LEADER" picomint-server-cli invite 2>/dev/null \
            | jq -r .invite_code) && [ -n "$invite" ] && [ "$invite" != "null" ]; then
            break
        fi
        invite=""
        sleep 1
    done

    if [ -z "$invite" ]; then
        echo "[$fed_name] DKG did not complete in time" >&2
        exit 1
    fi

    echo "    invite: $invite" >&2
    printf '%s' "$invite"
}

# Run DKG for each federation.
for idx in "${!FED_NAMES[@]}"; do
    INVITES[$idx]=$(setup_federation "${FED_NAMES[$idx]}" "${FED_PREFIXES[$idx]}")
done

echo "==> Waiting for gateway..."
until $DC exec -T gateway picomint-gateway-cli info >/dev/null 2>&1; do
    sleep 1
done

# Join + register + peg-in for each federation.
for idx in "${!FED_NAMES[@]}"; do
    fed_name="${FED_NAMES[$idx]}"
    prefix="${FED_PREFIXES[$idx]}"
    invite="${INVITES[$idx]}"

    echo "==> [$fed_name] Joining gateway to federation..."
    $DC exec -T gateway picomint-gateway-cli federation join "$invite"

    echo "==> [$fed_name] Registering gateway with federation..."
    for i in "${GUARDIANS[@]}"; do
        $DC exec -T "${prefix}-guardian-$i" picomint-server-cli module ln gateway add "$GATEWAY_URL"
    done

    echo "==> [$fed_name] Funding the gateway via federation peg-in..."
    federation_id=$($DC exec -T gateway picomint-gateway-cli federation list \
        | jq -r --arg name "$fed_name" '.federations[] | select(.federation_name == $name) | .federation_id')

    if [ -z "$federation_id" ] || [ "$federation_id" = "null" ]; then
        echo "[$fed_name] Could not resolve federation_id from gateway list" >&2
        exit 1
    fi

    deposit=$($DC exec -T gateway picomint-gateway-cli federation module wallet receive \
        --id "$federation_id" | jq -r .address)
    echo "    deposit address: $deposit"

    $DC exec -T bitcoind bitcoin-cli -regtest -rpcuser=bitcoin -rpcpassword=bitcoin \
        -rpcwallet=miner sendtoaddress "$deposit" 1 >/dev/null
    echo "    sent 1 BTC to gateway peg-in address"

    echo "==> [$fed_name] Waiting for the federation to mint ecash to the gateway..."
    bal=0
    for _ in $(seq 1 120); do
        bal=$($DC exec -T gateway picomint-gateway-cli federation balance --id "$federation_id" 2>/dev/null \
            | jq -r '.balance_msat // 0')
        if [ "${bal:-0}" -gt 0 ]; then
            break
        fi
        sleep 2
    done

    if [ "${bal:-0}" -eq 0 ]; then
        echo "[$fed_name] Gateway federation balance still zero — peg-in did not confirm in time" >&2
        exit 1
    fi
    echo "    gateway federation balance: $((bal / 1000)) sats"
done

cat <<EOF

==========================================================================
Setup complete.

  ${FED_NAMES[0]}  invite : ${INVITES[0]}
  ${FED_NAMES[1]} invite : ${INVITES[1]}

  Gateway URL       : $GATEWAY_URL   (registered with both federations)
  Recurring daemon  : http://localhost:8091
  ${FED_NAMES[0]}  UIs    : http://localhost:3000..3003
  ${FED_NAMES[1]} UIs    : http://localhost:3010..3013     (password: picomint)
  Bitcoind RPC      : http://localhost:18443        (user: bitcoin / pass: bitcoin)

The gateway holds federation ecash in both federations and can route
Lightning payments between clients. External LN routing is not
configured (no second LN node is bundled).

To reset everything: docker compose down -v
==========================================================================
EOF
