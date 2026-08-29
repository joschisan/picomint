#!/usr/bin/env bash
# One-shot installer for a picomint guardian on a fresh Ubuntu desktop.
#
# Installs Docker (if missing), brings up the bundled guardian + bitcoind
# compose, pins Guardian / Logs / Update shortcuts to the GNOME dock, then
# installs Signal Desktop for exchanging setup codes during the federation
# ceremony.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/joschisan/picomint/main/bootstrap/bootstrap.sh | bash

set -euo pipefail

DEPLOY_DIR="$HOME/picomint-guardian-daemon"
REF="${REF:-main}"
COMPOSE_URL="https://raw.githubusercontent.com/joschisan/picomint/$REF/bootstrap/docker-compose.yml"
UPDATE_URL="https://raw.githubusercontent.com/joschisan/picomint/$REF/bootstrap/update.sh"
UI_URL="http://127.0.0.1:3000"
LOGS_URL="http://127.0.0.1:3001"

confirm() {
    if [[ "${AUTO_YES:-}" == "1" ]]; then
        return 0
    fi
    read -rp "$1 [y/N] " reply </dev/tty
    [[ "$reply" =~ ^[Yy]$ ]]
}

install_launcher() {
    local id="$1" name="$2" exec_cmd="$3" icon="$4"

    mkdir -p "$HOME/.local/share/applications"

    cat > "$HOME/.local/share/applications/$id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$name
Exec=$exec_cmd
Icon=$icon
Terminal=false
EOF
}

pin_to_dock() {
    local id="$1"
    local favs

    favs=$(gsettings get org.gnome.shell favorite-apps 2>/dev/null || echo '[]')

    if [[ "$favs" == *"$id.desktop"* ]]; then
        return 0
    fi

    if [[ "$favs" == "[]" ]]; then
        favs="['$id.desktop']"
    else
        favs="${favs%]}, '$id.desktop']"
    fi

    gsettings set org.gnome.shell favorite-apps "$favs" 2>/dev/null || true
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
if [[ "$DISTRO_ID" != "ubuntu" || "$DISTRO_VERSION" != "26.04" ]]; then
    echo "This installer requires Ubuntu 26.04 LTS desktop. You appear to be running $DISTRO_ID $DISTRO_VERSION." >&2
    exit 1
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
  5. Pin Dashboard, Logs and Update shortcuts to the dock
  6. Install Signal Desktop for exchanging setup codes with co-guardians

EOF

confirm "Continue?" || { echo "Aborted."; exit 0; }

sudo -v

if ! command -v docker >/dev/null; then
    echo "==> Installing Docker"
    curl -fsSL https://get.docker.com | sh
fi

sudo usermod -aG docker "$USER"

echo "==> Preparing $DEPLOY_DIR"
mkdir "$DEPLOY_DIR"
cd "$DEPLOY_DIR"

echo "==> Downloading docker-compose.yml and update.sh"
curl -fsSL -O "$COMPOSE_URL"
curl -fsSL -O "$UPDATE_URL"
chmod +x update.sh

echo "==> Starting guardian"
sudo docker compose up -d

echo "==> Waiting for Web UI at $UI_URL"
for _ in $(seq 30); do
    if curl -sf "$UI_URL" >/dev/null; then
        break
    fi
    sleep 1
done

echo "==> Pinning shortcuts to the dock"
install_launcher picomint-guardian "Dashboard" "xdg-open $UI_URL" applications-internet
install_launcher picomint-guardian-logs "Logs" "xdg-open $LOGS_URL" utilities-terminal
install_launcher picomint-guardian-update "Update" "$DEPLOY_DIR/update.sh" system-software-update
pin_to_dock picomint-guardian
pin_to_dock picomint-guardian-logs
pin_to_dock picomint-guardian-update

if ! command -v signal-desktop >/dev/null; then
    echo "==> Installing Signal Desktop"
    curl -fsSL https://updates.signal.org/desktop/apt/keys.asc \
        | gpg --dearmor \
        | sudo tee /usr/share/keyrings/signal-desktop-keyring.gpg >/dev/null
    echo 'deb [arch=amd64 signed-by=/usr/share/keyrings/signal-desktop-keyring.gpg] https://updates.signal.org/desktop/apt xenial main' \
        | sudo tee /etc/apt/sources.list.d/signal-xenial.list >/dev/null
    sudo apt update
    sudo apt install -y signal-desktop
fi

echo "==> Pinning Signal Desktop to the dock"
pin_to_dock signal-desktop

cat <<EOF

Guardian is running.

  Web UI:   $UI_URL
  Logs UI:  $LOGS_URL
  Compose:  $DEPLOY_DIR/docker-compose.yml
  Logs:     sudo docker compose -f $DEPLOY_DIR/docker-compose.yml logs -f

The dock now has Dashboard, Logs and Update shortcuts — day-to-day
operation never needs a terminal again.

Next steps:
  1. Click Dashboard in the dock (or open $UI_URL).
  2. Open Signal and coordinate setup-code exchange with your co-guardians.
EOF
