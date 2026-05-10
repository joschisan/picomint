#!/usr/bin/env bash
# One-shot installer for a picomint guardian on a fresh Ubuntu desktop.
#
# Installs Docker (if missing), brings up the bundled guardian + bitcoind
# compose, opens the Web UI in a browser, then installs Signal Desktop for
# exchanging setup codes during the federation ceremony.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/joschisan/picomint/main/bootstrap.sh | bash

set -euo pipefail

DEPLOY_DIR="$HOME/picomint-guardian-daemon"
COMPOSE_URL="https://raw.githubusercontent.com/joschisan/picomint/main/docker-guardian/docker-compose.yml"
UI_URL="http://127.0.0.1:3000"

confirm() {
    if [[ "${AUTO_YES:-}" == "1" ]]; then
        return 0
    fi
    read -rp "$1 [y/N] " reply </dev/tty
    [[ "$reply" =~ ^[Yy]$ ]]
}

ARCH=$(dpkg --print-architecture)
if [[ "$ARCH" != "amd64" ]]; then
    echo "Unsupported architecture: $ARCH. This installer targets Ubuntu amd64." >&2
    exit 1
fi

DISTRO_ID="unknown"
DISTRO_VERSION="unknown"
if [[ -r /etc/os-release ]]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    DISTRO_ID="${ID:-unknown}"
    DISTRO_VERSION="${VERSION_ID:-unknown}"
fi
if [[ "$DISTRO_ID" != "ubuntu" ]]; then
    echo "This installer is tested on Ubuntu 26.04 LTS desktop. You appear to be running $DISTRO_ID $DISTRO_VERSION." >&2
    confirm "Continue anyway?" || { echo "Aborted."; exit 0; }
fi

if [[ -e "$DEPLOY_DIR" ]]; then
    echo "Existing deployment found at $DEPLOY_DIR. Aborting." >&2
    exit 1
fi

cat <<EOF
This installer will set up a picomint guardian on this machine:

  1. Install Docker (if missing)
  2. Download the guardian compose into $DEPLOY_DIR
  3. Start the guardian + a bundled, pruned Bitcoin Core node
  4. Wait for the Web UI to come up at $UI_URL
  5. Optionally install Signal Desktop for exchanging setup codes with co-guardians

EOF

confirm "Continue?" || { echo "Aborted."; exit 0; }

sudo -v

if ! command -v docker >/dev/null; then
    echo "==> Installing Docker"
    curl -fsSL https://get.docker.com | sh
fi

echo "==> Preparing $DEPLOY_DIR"
mkdir "$DEPLOY_DIR"
cd "$DEPLOY_DIR"

echo "==> Downloading docker-compose.yml"
curl -fsSL -O "$COMPOSE_URL"

echo "==> Starting guardian"
sudo docker compose up -d

echo "==> Waiting for Web UI at $UI_URL"
for _ in $(seq 30); do
    if curl -sf "$UI_URL" >/dev/null; then
        break
    fi
    sleep 1
done

if ! command -v signal-desktop >/dev/null; then
    echo
    if confirm "Install Signal Desktop for exchanging setup codes?"; then
        echo "==> Installing Signal Desktop"
        curl -fsSL https://updates.signal.org/desktop/apt/keys.asc \
            | gpg --dearmor \
            | sudo tee /usr/share/keyrings/signal-desktop-keyring.gpg >/dev/null
        echo 'deb [arch=amd64 signed-by=/usr/share/keyrings/signal-desktop-keyring.gpg] https://updates.signal.org/desktop/apt xenial main' \
            | sudo tee /etc/apt/sources.list.d/signal-xenial.list >/dev/null
        sudo apt update
        sudo apt install -y signal-desktop

        echo "==> Pinning Signal Desktop to the dock"
        favs=$(gsettings get org.gnome.shell favorite-apps 2>/dev/null || echo '[]')
        if [[ "$favs" != *signal-desktop.desktop* ]]; then
            if [[ "$favs" == "[]" ]]; then
                new="['signal-desktop.desktop']"
            else
                new="${favs%]}, 'signal-desktop.desktop']"
            fi
            gsettings set org.gnome.shell favorite-apps "$new" 2>/dev/null || true
        fi
    fi
fi

cat <<EOF

Guardian is running.

  Web UI:   $UI_URL
  Compose:  $DEPLOY_DIR/docker-compose.yml
  Logs:     sudo docker compose -f $DEPLOY_DIR/docker-compose.yml logs -f

Next steps:
  1. Open $UI_URL in your browser.
  2. Open Signal and coordinate setup-code exchange with your co-guardians.
EOF
