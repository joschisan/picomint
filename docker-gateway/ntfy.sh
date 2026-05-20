#!/usr/bin/env bash
# Pipe-driven ntfy.sh sender.
#
# Runs inside the picomint-gateway-daemon container (shipped at
# /usr/local/bin/ntfy.sh by the image). Reads the message body from
# stdin and POSTs it to an ntfy topic. If stdin is empty (e.g. an alert
# script decided no alert was needed), exits 0 without contacting the
# server. Pair with report.sh or any of the alert-*.sh scripts via
# `docker exec -i` so stdin is forwarded into this container.
#
# The free public server at https://ntfy.sh has no auth — pick a topic
# name that's unguessable like a password. Override --server for a
# self-hosted instance.
#
# Usage (from the host crontab):
#   docker exec    picomint-gateway-daemon report.sh                  --federation <ID> --since-hours <N> \
#     | docker exec -i picomint-gateway-daemon ntfy.sh --topic <TOPIC>
#   docker exec    picomint-gateway-daemon alert-federation-liquidity.sh --federation <ID> ... \
#     | docker exec -i picomint-gateway-daemon ntfy.sh --topic <TOPIC>

set -euo pipefail

TOPIC=""
SERVER="https://ntfy.sh"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --topic)   TOPIC="$2";   shift 2 ;;
        --server)  SERVER="$2";  shift 2 ;;
        *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
done

[[ -n "$TOPIC" ]] || { echo "missing --topic" >&2; exit 1; }

msg=$(cat)

[[ -n "$msg" ]] || exit 0

curl -fsS -d "$msg" "$SERVER/$TOPIC" >/dev/null
