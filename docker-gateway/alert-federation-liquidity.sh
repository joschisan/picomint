#!/usr/bin/env bash
# Balance alert for one federation on a picomint gateway.
#
# Runs inside the picomint-gateway-daemon container (shipped at
# /usr/local/bin/alert-federation-liquidity.sh by the image). Polls
# gateway-cli for the federation's ecash balance; if it sits below
# --min-balance-sat or above --max-balance-sat, writes a human-readable
# alert to stdout. Silent (no stdout) when the balance is inside the
# band — pipe the output through ntfy.sh and the server is only
# contacted when an alert fires.
#
# The two conditions are mutually exclusive (a balance can't be both
# below min and above max), so at most one alert is emitted per
# invocation. Intended for frequent cron invocation (e.g. every
# 30 minutes) — fires on every tick while out of band.
#
# Usage (from the host crontab):
#   docker exec picomint-gateway-daemon alert-federation-liquidity.sh \
#       --federation <ID> --min-balance-sat 100000 --max-balance-sat 10000000 \
#     | docker exec -i picomint-gateway-daemon ntfy.sh --topic <TOPIC>
#
# Threshold flags default to 0, which disables that check.

set -euo pipefail

FEDERATION=""
MIN_BALANCE_SAT=0
MAX_BALANCE_SAT=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --federation)       FEDERATION="$2";       shift 2 ;;
        --min-balance-sat)  MIN_BALANCE_SAT="$2";  shift 2 ;;
        --max-balance-sat)  MAX_BALANCE_SAT="$2";  shift 2 ;;
        *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
done

[[ -n "$FEDERATION" ]] || { echo "missing --federation" >&2; exit 1; }

cli() { picomint-gateway-cli "$@"; }

name=$(cli federation list | jq -r --arg f "$FEDERATION" '.federations[] | select(.federation==$f) | .federation_name')
[[ -n "$name" ]] || { echo "federation $FEDERATION not joined" >&2; exit 1; }

bal_sat=$(( $(cli federation balance --id "$FEDERATION" | jq -r '.balance_msat') / 1000 ))

emit_alert() {
    local title="$1" threshold_sat="$2"
    local current_btc threshold_btc ratio
    current_btc=$(awk   -v s="$bal_sat"       'BEGIN { printf "%.8f", s/100000000 }')
    threshold_btc=$(awk -v s="$threshold_sat" 'BEGIN { printf "%.8f", s/100000000 }')
    ratio=$(awk -v c="$bal_sat" -v t="$threshold_sat" \
        'BEGIN { if (t+0 == 0) printf "0.0"; else printf "%.1f", (c * 100.0) / t }')
    cat <<EOF
Alert: $title - $name
Current: $current_btc BTC
Threshold: $threshold_btc BTC
Ratio: ${ratio}%
EOF
}

if (( bal_sat < MIN_BALANCE_SAT )); then
    emit_alert "Balance Low" "$MIN_BALANCE_SAT"
elif (( MAX_BALANCE_SAT > 0 && bal_sat > MAX_BALANCE_SAT )); then
    emit_alert "Balance High" "$MAX_BALANCE_SAT"
fi
