#!/bin/bash

set -e

# Find the entrypoint script dynamically
ENTRYPOINT_SCRIPT=$(find /nix/store -type f -name '*-picomint-container-entrypoint.sh' | head -n 1)

if [[ -z "$ENTRYPOINT_SCRIPT" ]]; then
    echo "Error: picomint-container-entrypoint.sh not found in /nix/store" >&2
    exit 1
fi

echo "Waiting for Start9 config..."
while [ ! -f /start-os/start9/config.yaml ]; do
    sleep 1
done

echo "Config file found at /start-os/start9/config.yaml"

export DATA_DIR=/picomint
export BITCOIN_NETWORK=bitcoin
export UI_ADDR=0.0.0.0:3000

# Config file structure:
# picomint-bitcoin-backend:
#   backend-type: <bitcoind|esplora>
#   user: <username>           # only for bitcoind
#   password: <password>       # only for bitcoind
#   url: 'https://...'         # only for esplora

# Parse configuration using yq
BACKEND_TYPE=$(yq '.picomint-bitcoin-backend.backend-type' /start-os/start9/config.yaml)

if [ "$BACKEND_TYPE" = "bitcoind" ]; then
    echo "Using Bitcoin Core backend"
    BITCOIN_USER=$(yq '.picomint-bitcoin-backend.user' /start-os/start9/config.yaml)
    BITCOIN_PASS=$(yq '.picomint-bitcoin-backend.password' /start-os/start9/config.yaml)

    if [ -z "$BITCOIN_USER" ] || [ -z "$BITCOIN_PASS" ]; then
        echo "ERROR: Could not parse Bitcoin RPC credentials from config"
        exit 1
    fi

    export BITCOIND_URL="http://bitcoind.embassy:8332"
    export BITCOIND_USERNAME="${BITCOIN_USER}"
    export BITCOIND_PASSWORD="${BITCOIN_PASS}"

    echo "Starting Picomint with Bitcoin Core at $BITCOIND_URL"
elif [ "$BACKEND_TYPE" = "esplora" ]; then
    echo "Using Esplora backend"
    ESPLORA_URL=$(yq '.picomint-bitcoin-backend.url' /start-os/start9/config.yaml)

    if [ -z "$ESPLORA_URL" ]; then
        echo "ERROR: Could not parse Esplora URL from config"
        exit 1
    fi

    export ESPLORA_URL
    echo "Starting Picomint with Esplora at $ESPLORA_URL"
else
    echo "ERROR: Unknown backend type: $BACKEND_TYPE"
    exit 1
fi

# Read and set password
UI_PASSWORD=$(yq '.password' /start-os/start9/config.yaml)
export UI_PASSWORD
echo "UI_PASSWORD is set"

# Read and set RUST_LOG from config
RUST_LOG_LEVEL=$(yq '.advanced.rust-log-level' /start-os/start9/config.yaml)
export RUST_LOG="${RUST_LOG_LEVEL}"
echo "Setting RUST_LOG=${RUST_LOG}"

# Create .backupignore to exclude files that shouldn't be backed up:
#
# We exclude the active database because:
# - `database/` is the live database that may be in an inconsistent state during backup
# - Backing up active databases can lead to corruption
#
# Instead, we rely on `db_checkpoints/` which contains:
# - Periodic consistent snapshots of the federation state
# - Safe restore points that allow rejoining the federation
# - Much faster sync than starting from genesis (session 0)
if [ ! -f /picomint/.backupignore ]; then
    echo "Creating .backupignore file..."
    cat > /picomint/.backupignore <<EOF
database
database.db.lock
EOF
fi

# Check if we need to restore from checkpoint (after a backup restore)
if [ ! -d "/picomint/database" ] && [ -d "/picomint/db_checkpoints" ]; then
    echo "Database directory not found, checking for restore from checkpoint..."

    # Find the single checkpoint directory (there should only be one)
    CHECKPOINT=$(ls -1 /picomint/db_checkpoints)

    if [ -n "$CHECKPOINT" ]; then
        echo "Found checkpoint: $CHECKPOINT"
        echo "Restoring database from checkpoint..."

        # Create the database directory and copy checkpoint files
        mkdir -p /picomint/database
        cp -r /picomint/db_checkpoints/"$CHECKPOINT"/* /picomint/database/

        echo "Database restored from checkpoint $CHECKPOINT"
    else
        echo "No checkpoint found to restore from"
    fi
else
    echo "Database directory exists, proceeding with normal startup"
fi

exec bash "$ENTRYPOINT_SCRIPT" "$@"
