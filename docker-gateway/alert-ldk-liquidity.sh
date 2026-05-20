#!/usr/bin/env bash
# Low-channel-capacity alert for the gateway's LDK node.
#
# Runs inside the picomint-gateway-daemon container (shipped at
# /usr/local/bin/alert-ldk-liquidity.sh by the image). Polls gateway-cli
# for total inbound/outbound channel capacity across all usable channels.
# If either side sits below its threshold, writes a human-readable alert
# to stdout. When both sides trip simultaneously the script emits both
# alert blocks in a single message separated by a blank line.
#
# Silent (no stdout) when both sides are above threshold — pipe through
# ntfy.sh and the server is only contacted when an alert fires.
#
# Gateway-wide rather than per-federation: LDK channels are shared
# across every fed the gateway serves. Run one cron entry total.
#
# Usage (from the host crontab):
#   docker exec picomint-gateway-daemon alert-ldk-liquidity.sh \
#       --min-outbound-sat 5000000 --min-inbound-sat 5000000 \
#     | docker exec -i picomint-gateway-daemon ntfy.sh --topic <TOPIC>
#
# Threshold flags default to 0, which disables that check.

set -euo pipefail

MIN_OUTBOUND_SAT=0
MIN_INBOUND_SAT=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --min-outbound-sat)  MIN_OUTBOUND_SAT="$2";  shift 2 ;;
        --min-inbound-sat)   MIN_INBOUND_SAT="$2";   shift 2 ;;
        *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
done

ldk=$(picomint-gateway-cli ldk balances)
out_sat=$(( $(echo "$ldk" | jq -r '.total_outbound_capacity_msat') / 1000 ))
in_sat=$(( $(echo  "$ldk" | jq -r '.total_inbound_capacity_msat')  / 1000 ))

alerts=()

emit_alert() {
    local title="$1" current_sat="$2" threshold_sat="$3"
    local current_btc threshold_btc ratio
    current_btc=$(awk   -v s="$current_sat"   'BEGIN { printf "%.8f", s/100000000 }')
    threshold_btc=$(awk -v s="$threshold_sat" 'BEGIN { printf "%.8f", s/100000000 }')
    ratio=$(awk -v c="$current_sat" -v t="$threshold_sat" \
        'BEGIN { if (t+0 == 0) printf "0.0"; else printf "%.1f", (c * 100.0) / t }')
    cat <<EOF
Alert: $title
Current: $current_btc BTC
Threshold: $threshold_btc BTC
Ratio: ${ratio}%
EOF
}

if (( out_sat < MIN_OUTBOUND_SAT )); then
    alerts+=("$(emit_alert "Outbound Liquidity Low" "$out_sat" "$MIN_OUTBOUND_SAT")")
fi
if (( in_sat < MIN_INBOUND_SAT )); then
    alerts+=("$(emit_alert "Inbound Liquidity Low" "$in_sat" "$MIN_INBOUND_SAT")")
fi

if (( ${#alerts[@]} == 0 )); then
    exit 0
fi

# Join alert blocks with a blank line so a single ntfy message contains
# both when both sides trip simultaneously.
first=1
for a in "${alerts[@]}"; do
    if (( first )); then
        first=0
    else
        printf '\n'
    fi
    printf '%s\n' "$a"
done
