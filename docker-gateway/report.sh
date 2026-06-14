#!/usr/bin/env bash
# Summary report for one federation on a picomint gateway.
#
# Runs inside the picomint-gateway-daemon container (shipped at
# /usr/local/bin/report.sh by the image). Queries the daemon's
# analytics.sqlite (outgoing_payments + incoming_payments views) and
# gateway-cli (the federation's ecash balance), formats a human-readable
# digest, and writes it to stdout. Pipe through ntfy.sh to deliver, or
# run standalone to print on the console while iterating on the format.
#
# Usage (from the host crontab):
#   docker exec picomint-gateway-daemon report.sh --federation <ID> --since-hours <N> \
#     | docker exec -i picomint-gateway-daemon ntfy.sh --topic <TOPIC>

set -euo pipefail

FEDERATION=""
SINCE_HOURS=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --federation)   FEDERATION="$2";   shift 2 ;;
        --since-hours)  SINCE_HOURS="$2";  shift 2 ;;
        *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
done

[[ -n "$FEDERATION"  ]] || { echo "missing --federation"  >&2; exit 1; }
[[ -n "$SINCE_HOURS" ]] || { echo "missing --since-hours" >&2; exit 1; }

cli() { picomint-gateway-cli "$@"; }
sql() { sqlite3 -json "$DATA_DIR/analytics/analytics.sqlite" "$1"; }

# msat -> "X.XX" sat string (2 decimals, full msat precision rounded)
msat_to_sat() { awk -v m="$1" 'BEGIN { printf "%.2f", m/1000 }'; }
# msat -> "0.XXXXXXXX" BTC string (8 decimals)
msat_to_btc() { awk -v m="$1" 'BEGIN { printf "%.8f", m/100000000000 }'; }

name=$(cli federation list | jq -r --arg f "$FEDERATION" '.federations[] | select(.federation==$f) | .federation_name')
[[ -n "$name" ]] || { echo "federation $FEDERATION not joined" >&2; exit 1; }

since_ms=$(( ($(date +%s) - SINCE_HOURS * 3600) * 1000 ))

outgoing=$(sql "
SELECT
    COUNT(*) FILTER (WHERE status='success')   AS succeeded,
    COUNT(*) FILTER (WHERE status='cancelled') AS failed,
    COALESCE(CAST(AVG(completed_at - started_at) FILTER (WHERE status='success') AS INTEGER), 0) AS avg_latency_ms,
    COALESCE(SUM(amount_msat)        FILTER (WHERE status='success'), 0) AS volume_msat,
    COALESCE(SUM(gw_fee_msat)        FILTER (WHERE status='success'), 0) AS gw_fee_msat,
    COALESCE(SUM(tx_fee_msat)        FILTER (WHERE status='success'), 0) AS tx_fee_msat,
    COALESCE(SUM(ln_fee_budget_msat) FILTER (WHERE status='success'), 0) AS ln_fee_budget_msat,
    COALESCE(SUM(ln_fee_paid_msat)   FILTER (WHERE status='success'), 0) AS ln_fee_paid_msat,
    COALESCE(SUM(ln_fee_kept_msat)   FILTER (WHERE status='success'), 0) AS ln_fee_kept_msat
FROM outgoing_payments
WHERE started_at > $since_ms AND federation = '$FEDERATION';
" | jq -c '.[0]')

incoming=$(sql "
SELECT
    COUNT(*) FILTER (WHERE status='success')                  AS succeeded,
    COUNT(*) FILTER (WHERE status IN ('failure','refunded'))  AS failed,
    COALESCE(CAST(AVG(completed_at - started_at) FILTER (WHERE status='success') AS INTEGER), 0) AS avg_latency_ms,
    COALESCE(SUM(amount_msat) FILTER (WHERE status='success'), 0) AS volume_msat,
    COALESCE(SUM(gw_fee_msat) FILTER (WHERE status='success'), 0) AS gw_fee_msat,
    COALESCE(SUM(tx_fee_msat) FILTER (WHERE status='success'), 0) AS tx_fee_msat
FROM incoming_payments
WHERE started_at > $since_ms AND federation = '$FEDERATION';
" | jq -c '.[0]')

balance_btc=$(msat_to_btc "$(cli federation balance --id "$FEDERATION" | jq -r '.balance_msat')")

out_succeeded=$(echo   "$outgoing" | jq -r '.succeeded')
out_failed=$(echo      "$outgoing" | jq -r '.failed')
out_latency=$(echo     "$outgoing" | jq -r '.avg_latency_ms')
out_volume_btc=$(msat_to_btc "$(echo "$outgoing" | jq -r '.volume_msat')")
out_fee_sat=$(msat_to_sat    "$(echo "$outgoing" | jq -r '.gw_fee_msat')")
out_tx_fee_sat=$(msat_to_sat "$(echo "$outgoing" | jq -r '.tx_fee_msat')")
out_ln_budget_msat=$(echo "$outgoing" | jq -r '.ln_fee_budget_msat')
out_ln_paid_msat=$(echo   "$outgoing" | jq -r '.ln_fee_paid_msat')
out_ln_budget_sat=$(msat_to_sat "$out_ln_budget_msat")
out_ln_paid_sat=$(msat_to_sat   "$out_ln_paid_msat")
out_ln_kept_sat=$(msat_to_sat   "$(echo "$outgoing" | jq -r '.ln_fee_kept_msat')")
out_ln_util=$(awk -v p="$out_ln_paid_msat" -v b="$out_ln_budget_msat" \
    'BEGIN { if (b+0 == 0) printf "0.0"; else printf "%.1f", (p * 100.0) / b }')

in_succeeded=$(echo  "$incoming" | jq -r '.succeeded')
in_failed=$(echo     "$incoming" | jq -r '.failed')
in_latency=$(echo    "$incoming" | jq -r '.avg_latency_ms')
in_volume_btc=$(msat_to_btc "$(echo "$incoming" | jq -r '.volume_msat')")
in_fee_sat=$(msat_to_sat    "$(echo "$incoming" | jq -r '.gw_fee_msat')")
in_tx_fee_sat=$(msat_to_sat "$(echo "$incoming" | jq -r '.tx_fee_msat')")

cat <<EOF
$name — ${SINCE_HOURS}h
Balance: $balance_btc BTC

Outgoing
Payments: $out_succeeded ($out_failed failed)
Average Latency: ${out_latency}ms
Volume: $out_volume_btc BTC
Fee: $out_fee_sat sat
Ln Fee: $out_ln_budget_sat sat
Ln Fee Paid: $out_ln_paid_sat sat
Ln Fee Kept: $out_ln_kept_sat sat
Ln Fee Utilization: ${out_ln_util}%
Federation Fee: $out_tx_fee_sat sat

Incoming
Payments: $in_succeeded ($in_failed failed)
Average Latency: ${in_latency}ms
Volume: $in_volume_btc BTC
Fee: $in_fee_sat sat
Federation Fee: $in_tx_fee_sat sat
EOF
