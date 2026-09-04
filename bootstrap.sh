#!/usr/bin/env bash
# One-shot installer for a picomint node on a fresh Ubuntu desktop.
#
# Installs Docker (if missing), brings up the bundled node + a fully
# validating bitcoind, installs Signal Desktop for exchanging setup codes
# during the mint ceremony, and pins Dashboard, Logs and Update
# shortcuts to the dock. Nothing here needs a terminal afterwards.
#
# Fully self-contained — the compose file, updater and log viewer are
# embedded below and written to $DEPLOY_DIR. Safe to re-run at any time:
# every step is idempotent and node state lives in Docker volumes a
# re-run never touches.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/joschisan/picomint/main/bootstrap.sh | bash

set -euo pipefail

DEPLOY_DIR="$HOME/picomint"
UI_URL="http://127.0.0.1:3000"

confirm() {
    if [[ "${AUTO_YES:-}" == "1" ]]; then
        return 0
    fi
    read -rp "$1 [y/N] " reply </dev/tty
    [[ "$reply" =~ ^[Yy]$ ]]
}

install_launcher() {
    local id="$1" name="$2" exec_cmd="$3" icon="$4" terminal="${5:-false}"

    mkdir -p "$HOME/.local/share/applications"

    cat > "$HOME/.local/share/applications/$id.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=$name
Exec=$exec_cmd
Icon=$icon
Terminal=$terminal
EOF

    pin_to_dock "$id"
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

# A full, unpruned bitcoind needs ~1TB, plus headroom for the node's own
# database and future chain growth.
AVAIL_GB=$(df -BG --output=avail "$HOME" | tail -1 | tr -dc '0-9')
if [[ "$AVAIL_GB" -lt 1200 ]]; then
    echo "Only ${AVAIL_GB}GB free on $HOME. A full Bitcoin Core node needs ~1TB, and 1.2TB is recommended." >&2
    confirm "Continue anyway?" || { echo "Aborted."; exit 0; }
fi

cat <<EOF
This installer will set up a picomint node on this machine:

  1. Install Docker (if missing)
  2. Write the node compose, updater and log viewer into $DEPLOY_DIR
  3. Pull and start the node + a bundled, fully validating Bitcoin Core node (~1TB)
  4. Wait for the Web UI to come up at $UI_URL
  5. Pin Dashboard, Logs and Update shortcuts to the dock
  6. Install Signal Desktop for exchanging setup codes with co-operators

It is safe to re-run this installer at any time.

EOF

confirm "Continue?" || { echo "Aborted."; exit 0; }

sudo -v

if ! command -v docker >/dev/null; then
    echo "==> Installing Docker"
    sudo apt update
    sudo apt install -y docker.io docker-compose-v2
fi

echo "==> Writing $DEPLOY_DIR"
mkdir -p "$DEPLOY_DIR"
cd "$DEPLOY_DIR"

cat > docker-compose.yml <<'COMPOSE'
# Both services run on the host network. The node's p2p and client api
# share one iroh endpoint (UDP), and stacking Docker's NAT on top of the
# router's would give iroh two layers to punch through instead of one. That
# means each service binds its own address rather than being contained by a
# published port, so the loopback binds below are what keep the Web UI and
# the Bitcoin RPC off the LAN.
services:
  picomint-node-daemon:
    image: ghcr.io/joschisan/picomint-node-daemon:main
    container_name: picomint-node-daemon
    restart: always
    network_mode: host
    # Logs go to the system journal: the desktop user can read them without
    # docker privileges, and they survive container recreation on update.
    logging:
      driver: journald
    depends_on:
      - bitcoind
    volumes:
      - picomint_node_daemon_data:/data
    environment:
      - DATA_DIR=/data
      - BITCOIND_URL=http://bitcoin:bitcoin@127.0.0.1:8332
      # The iroh endpoint must be reachable from the internet for nodes and
      # clients to talk to your node.
      - P2P_ADDR=0.0.0.0:8080
      # Web UI — loopback only, reachable from a browser on this machine and
      # nowhere else. Do not change this to 0.0.0.0: on the host network that
      # puts node administration on your LAN.
      - UI_ADDR=127.0.0.1:3000

  bitcoind:
    image: bitcoin/bitcoin:latest
    container_name: bitcoind
    restart: always
    network_mode: host
    # Logs go to the system journal: the desktop user can read them without
    # docker privileges, and they survive container recreation on update.
    logging:
      driver: journald
    volumes:
      - bitcoind_data:/home/bitcoin/.bitcoin
    command:
      - -server=1
      # RPC is loopback only — the node shares this network namespace, and
      # nothing else needs to reach it.
      - -rpcbind=127.0.0.1
      - -rpcallowip=127.0.0.1
      - -rpcuser=bitcoin
      - -rpcpassword=bitcoin
      - -dbcache=1024

volumes:
  picomint_node_daemon_data:
  bitcoind_data:
COMPOSE

cat > update.sh <<'UPDATE'
#!/usr/bin/env bash
# Node updater, launched in a terminal from the "Update" icon installed by
# bootstrap.sh. Pulls the newest images for the deployed compose and recreates
# any containers whose image changed.

set -euo pipefail

# Hold the window open on success and failure alike, so the operator can read
# the outcome before the terminal closes.
trap 'echo; read -rp "Press enter to close this window."' EXIT

cd "$HOME/picomint"

echo "Enter password to update Picomint Node and Bitcoind to the latest release."
echo

sudo docker compose pull
sudo docker compose up -d

echo
echo "Update successful."
UPDATE

cat > logs.sh <<'LOGS'
#!/usr/bin/env bash
# Live node log viewer, launched from the "Logs" icon installed by
# bootstrap.sh. Reads the system journal the containers log to — no docker
# privileges needed. Read-only; close the window when done.

exec journalctl -f -n 200 CONTAINER_NAME=picomint-node-daemon
LOGS

chmod +x update.sh logs.sh

echo "==> Pulling images"
sudo docker compose pull

echo "==> Starting node"
sudo docker compose up -d

echo "==> Waiting for Web UI at $UI_URL"
for _ in $(seq 60); do
    if curl -sf "$UI_URL" >/dev/null; then
        break
    fi
    sleep 1
done

echo "==> Pinning shortcuts to the dock"
install_launcher picomint-node "Dashboard" "xdg-open $UI_URL" web-browser
install_launcher picomint-node-logs "Logs" "$DEPLOY_DIR/logs.sh" utilities-system-monitor true
install_launcher picomint-node-update "Update" "$DEPLOY_DIR/update.sh" system-software-update true

# Unconditional so a re-run also upgrades an existing install — Signal
# builds refuse to connect 90 days after being built.
echo "==> Installing Signal Desktop"
curl -fsSL https://updates.signal.org/desktop/apt/keys.asc \
    | gpg --dearmor \
    | sudo tee /usr/share/keyrings/signal-desktop-keyring.gpg >/dev/null
echo 'deb [arch=amd64 signed-by=/usr/share/keyrings/signal-desktop-keyring.gpg] https://updates.signal.org/desktop/apt xenial main' \
    | sudo tee /etc/apt/sources.list.d/signal-xenial.list >/dev/null
sudo apt update
sudo apt install -y signal-desktop

echo "==> Pinning Signal Desktop to the dock"
pin_to_dock signal-desktop

cat <<EOF

Node is running.

Next steps:
  1. Click Dashboard in the dock (or open $UI_URL).
  2. Open Signal and coordinate setup-code exchange with your co-operators.

The dock also has Logs for the node's log output and Update for
installing future releases — day-to-day operation never needs a terminal
again.
EOF
