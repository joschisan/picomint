#!/usr/bin/env bash
# Graphical updater for a picomint guardian, launched from the "Update" icon
# installed by bootstrap.sh. Pulls the newest images for the deployed compose
# and recreates any containers whose image changed.

set -euo pipefail

DEPLOY_DIR="$HOME/picomint-guardian-daemon"

info() { zenity --info --width=420 --title="Update" --text="$1"; }
die() { zenity --error --width=420 --title="Update" --text="$1" || true; exit 1; }

if [[ ! -f "$DEPLOY_DIR/docker-compose.yml" ]]; then
    die "No guardian deployment found at $DEPLOY_DIR."
fi

cd "$DEPLOY_DIR"

# sg applies docker-group membership granted after this desktop session began.
if ! sg docker -c "docker compose pull && docker compose up -d" 2>&1 \
    | zenity --progress --pulsate --auto-close --no-cancel --width=460 \
        --title="Update" --text="Pulling the latest release…"; then
    die "The update did not complete. Your guardian may still be running the previous release."
fi

info "Your guardian is up to date."
