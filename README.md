# Picomint - Alpha

A minimal implementation of a federated Chaumian ecash mint on Bitcoin.

## Deploy Node

Nodes run on a fresh **Ubuntu 26.04 LTS desktop** (amd64) with a screen and keyboard:

```bash
curl -fsSL https://raw.githubusercontent.com/joschisan/picomint/main/bootstrap.sh | bash
```

The installer is fully self-contained — the compose file, updater and log viewer are embedded in the script and written to `~/picomint`. It installs Docker (if missing), brings up the node + a bundled bitcoind, pins Dashboard / Logs / Update shortcuts to the dock, and installs Signal Desktop for exchanging setup codes with co-operators. It is safe to re-run at any time; node state lives in Docker volumes a re-run never touches. CI runs the bootstrap end-to-end on GitHub Actions' `ubuntu-26.04` runner.

### Bitcoin Backend

The node runs as a lightweight daemon on top of a local **unpruned** Bitcoin Core node. The bundled compose starts one for you alongside the node. Any machine that can comfortably run Bitcoin Core can run the picomint node on top — picomint's own resource footprint is negligible compared to Core's.

Pruning is not supported: a halted mint must be able to resume from blocks that may pre-date a rolling prune window.

Initial block download pulls the full chain over the network, so expect the first boot on mainnet to take a long time and several hundred GB of bandwidth and disk. The node will sit idle until bitcoind catches up.

### Accessing the CLI

The `picomint-node-cli` binary is included in the container and on the `PATH`. Run CLI commands from the host like:

```bash
sudo docker exec picomint-node-daemon picomint-node-cli --help
```

The walkthroughs below use the bare `picomint-node-cli …` form — prefix with `sudo docker exec picomint-node-daemon` to run them.

### Setup Ceremony

Before the mint can start processing transactions, nodes run a one-time setup ceremony. The Web UI walks you through it in a setup wizard; the CLI does the same thing.

Exactly one node sets the global mint config and passes `--mint-name` and `--mint-size`; the others pass only their own `<name>`:

```bash
picomint-node-cli setup set-local-params <name> [--mint-name X] [--mint-size N]
```

`set-local-params` returns a setup code. Every node then calls `add-node` once per node with that node's setup code:

```bash
picomint-node-cli setup add-node <setup-code>
```

Once every node has added every node, everyone runs:

```bash
picomint-node-cli setup start-dkg
```

Check your progress with:

```bash
picomint-node-cli setup status
```

### Invite Users

Users add the mint with an invite code and any node can create one:

```bash
picomint-node-cli invite
```

The client can use this invite to download and verify the mint config from the node that generated it.

### Configure Gateways

The mint maintains an explicit list of recommended Lightning gateways. Any node can add a gateway and clients will priorititze gateways by the number of nodes recommending them.

Add a gateway:

```bash
picomint-node-cli module lightning gateway add <url>
```

Remove one:

```bash
picomint-node-cli module lightning gateway remove <url>
```

List the current recommendations:

```bash
picomint-node-cli module lightning gateway list
```

### Backup

Once the setup ceremony completes, save your node's config to a file on
your local machine and stash it somewhere safe (encrypted backup, password
manager, paper printout):

```bash
picomint-node-cli config > config.json
```

This single file is the only state you need to keep. It contains your
node's secret keys plus the mint's consensus config. The live
`database.redb` is operational state (BFT sessions, block sync) which is
reconstructed from nodes when a restored node rejoins.

If your deployment is ever lost, copy the backup back into a fresh container:

```bash
sudo docker cp config.json picomint-node-daemon:/tmp/config.json
```

And run `setup restore`:

```bash
picomint-node-cli setup restore /tmp/config.json
```

### Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Iroh endpoint                | Yes             |
| 3000 | Web UI (setup + dashboard)   | Localhost only  |

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no
network exposure. Reach it with `sudo docker exec picomint-node-daemon
picomint-node-cli …`.

### Configuration

| Env                          | Required | Default           | Description                                |
|------------------------------|----------|-------------------|--------------------------------------------|
| `DATA_DIR`                   | yes      |                   | Directory for the database file            |
| `BITCOIN_NETWORK`            | no       | `bitcoin`         | `bitcoin`, `testnet`, `signet`, `regtest`  |
| `BITCOIND_URL`               | yes      |                   | Bitcoin Core RPC URL with embedded credentials, e.g. `http://user:pass@127.0.0.1:8332`. Must point at an **unpruned** node — see [Bitcoin Backend](#bitcoin-backend) above. |
| `P2P_ADDR`                   | no       | `0.0.0.0:8080`    | Iroh endpoint listen address               |
| `UI_ADDR`                    | no       | `127.0.0.1:3000`  | Web UI listen address                      |

## Deploy Gateway

The gateway is a single container image: `ghcr.io/joschisan/picomint-gateway-daemon:main`. Set it up with Docker however you prefer — persist `/data` in a volume, publish the public API port `8080` and the LDK Lightning P2P port `9735`, and configure it through the environment variables documented in [Configuration](#configuration-1) below.

### Accessing the CLI

The `picomint-gateway-cli` binary is included in the container and on the `PATH`. Run CLI commands from the host like:

```bash
sudo docker exec picomint-gateway-daemon picomint-gateway-cli --help
```

The walkthroughs below use the bare `picomint-gateway-cli …` form — prefix with `sudo docker exec picomint-gateway-daemon` to run them.

A first call to confirm everything is wired up:

```bash
picomint-gateway-cli info
```

Your info will look like

```json
{
  "public_key": "02abfe4a99f1ed8f67c1f07e5d47f3ab3d2e9c5b8a1c8e7f2a6d4b7e9c1f5a3e8d",
  "alias": "picomint-gateway-daemon",
  "network": "bitcoin",
  "block_height": 842195,
  "synced_to_chain": true
}
```

### Open Channels

To route payments on behalf of mints the gateway needs Lightning channels — specifically inbound liquidity, since a fresh node cannot receive payments. The usual approach is to buy an inbound channel from a Lightning Service Provider (LSP) such as [LN Big](https://lnbig.com). LSPs will ask for the node's `public_key` from `info` above and may require you to connect to them before they open the channel:

```bash
picomint-gateway-cli ldk node connect <lsp-pubkey> <lsp-host>
```

You can also open outbound channels yourself but first the gateway's embedded LDK node needs onchain bitcoin to open channels. Generate a receive address:

```bash
picomint-gateway-cli ldk onchain receive
```

Send bitcoin to it, then check the result:

```bash
picomint-gateway-cli ldk balances
```

Once the onchain balance is available connect to a node and open a channel with

```bash
picomint-gateway-cli ldk channel open <pubkey> <host> <channel-size-sat>
```

Running a second outbound channel alongside the LSP's inbound one is worthwhile: with only one channel, outgoing payments can fail once user balances drain toward the counterparty's channel reserve. Monitor channel state with:

```bash
picomint-gateway-cli ldk channel list
```

### Add Mints

The gateway can serve multiple mints simultaneously. Add one with an invite code (see [Invite Users](#invite-users) above for how nodes produce these):

```bash
picomint-gateway-cli mint add <invite>
```

List added mints:

```bash
picomint-gateway-cli mint list
```

Remove a mint and delete all of its data:

```bash
picomint-gateway-cli mint remove <mint-id>
```

This is destructive: check for in-flight payments via `query` first, otherwise you might lose funds.

For the gateway to actually route payments on behalf of a mint, its nodes also need to add the gateway's URL to their recommended list — see [Configure Gateways](#configure-gateways) above.

### Manage Mint Liquidity

Every command below accepts `--id <mint-id>` to target a specific mint. When exactly one mint is added (the common case) the flag can be omitted and that mint is used.

The gateway holds its own ecash balance in every mint it has added. Check it with:

```bash
picomint-gateway-cli mint balance
```

You can move funds in and out either onchain or as an ecash string.

**Receive Onchain:** generate a mint deposit address and send bitcoin to it. When the transaction confirms the mint issues ecash to the gateway.

```bash
picomint-gateway-cli mint module onchain receive
```

**Send Onchain:** burn ecash in exchange for an onchain transfer to the given address. The mint picks a feerate; check what it will charge first:

```bash
picomint-gateway-cli mint module onchain send-fee
```

Then send:

```bash
picomint-gateway-cli mint module onchain send <address> <amount>
```

Passing `--fee <amount>` overrides the feerate with an exact value; otherwise whatever `send-fee` currently reports is used.

**Send Ecash:** spend part of the mint balance as a base32-encoded ecash string you can hand to another client:

```bash
picomint-gateway-cli mint module ecash send <amount>
```

**Receive Ecash:** reissue an ecash string produced by `mint send` (on this gateway or any other client) into your balance:

```bash
picomint-gateway-cli mint module ecash receive <ecash>
```

### Restore

If your gateway deployment is ever corrupted you can restore your onchain funds and ecash from your twelve word mnemonic:

```bash
picomint-gateway-cli mnemonic
```

The mnemonic can be used with any Bip 39 compatible wallet to restore the onchain funds and with any Picomint wallet to restore the funds in the mints.  **The balance in your open lightning channels is lost.**

### Analytics

The gateway mirrors every gateway-module event into a SQLite database at
`{DATA_DIR}/analytics/analytics.sqlite`. The directory is **wiped on every
startup** and rebuilt by replaying the event log — analytics are derived,
not authoritative, so it's safe to delete and let it rebuild.

Activity is exposed through two per-direction views,
`outgoing_payments` and `incoming_payments`. Each row is one operation,
joined across the underlying event tables. They are kept separate
because the outgoing side carries an LN-routing-fee budget that doesn't
exist on the incoming side — a single unified view would mean three
permanent NULL columns on every incoming row.

Query it with read-only SQL through the admin CLI — the daemon runs the
query against the live db and returns one JSON object per row, the same
shape `sqlite3 --json` prints. Ten most recent outgoing payments:

```bash
picomint-gateway-cli query \
    "SELECT * FROM outgoing_payments ORDER BY started_at DESC LIMIT 10"
```

Status breakdown for outgoing:

```bash
picomint-gateway-cli query \
    "SELECT status, COUNT(*) AS n FROM outgoing_payments GROUP BY status"
```

Total outgoing volume per mint, in sat:

```bash
picomint-gateway-cli query \
    "SELECT mint, SUM(amount_msat)/1000 AS sat \
     FROM outgoing_payments WHERE status='success' GROUP BY mint"
```

**Columns common to both views:**

| Column           | Type    | Notes                                                                  |
|------------------|---------|------------------------------------------------------------------------|
| `mint`     | TEXT    | Hex-encoded mint id                                              |
| `operation`      | TEXT    | Hex-encoded operation id; unique within the view                       |
| `status`         | TEXT    | See below                                                              |
| `started_at`     | INTEGER | When the operation was initiated (ms since epoch)                      |
| `completed_at`   | INTEGER | NULL while `status = 'pending'`                                        |
| `amount_msat`    | INTEGER | Payment amount, msat                                                   |
| `gateway_fee_msat`    | INTEGER | The gateway's fee cut                                                  |
| `tx_fee_msat`    | INTEGER | Mint consensus tx fee (NULL until the tx lands)                  |
| `tx_remint_msat` | INTEGER | Mint tx remint amount                                            |
| `tx_txid`        | TEXT    | Mint tx id (NULL until the tx lands)                             |
| `preimage`       | TEXT    | Hex-encoded; NULL unless `status = 'success'`                          |

**Additional columns on `outgoing_payments`:**

| Column             | Type    | Notes                                                                  |
|--------------------|---------|------------------------------------------------------------------------|
| `gateway_fee_kept_msat` | INTEGER | `gateway_fee_msat` less the realized LN routing cost (NULL while pending; 0 if cancelled) |

**Status values:**

- `outgoing_payments`: `pending`, `success`, `cancelled`
- `incoming_payments`: `pending`, `success`, `failure`, `refunded`

The raw event tables (`send`, `send_success`, `send_cancel`, `receive`,
`receive_success`, `receive_failure`, `receive_refund`, `tx_create`) are
also queryable if you need a finer view.

### Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Public API (HTTP)            | Yes             |
| 9735 | LDK Lightning P2P (BOLT)     | Yes             |

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no
network exposure. Reach it with `sudo docker exec picomint-gateway-daemon
picomint-gateway-cli …`.

### Configuration

| Env                        | Required | Default           | Description                                 |
|----------------------------|----------|-------------------|---------------------------------------------|
| `DATA_DIR`                 | yes      |                   | Directory for the database + LDK node data  |
| `BITCOIN_NETWORK`          | no       | `bitcoin`         | `bitcoin`, `testnet`, `signet`, `regtest`   |
| `ESPLORA_URL`              | one of   |                   | Esplora HTTP URL                            |
| `BITCOIND_URL`             | one of   |                   | Bitcoin Core RPC URL with embedded credentials, e.g. `http://user:pass@127.0.0.1:8332` |
| `API_ADDR`                 | no       | `0.0.0.0:8080`    | Public API listen address                   |
| `LDK_ADDR`                 | no       | `0.0.0.0:9735`    | LDK Lightning P2P listen address (BOLT)     |
| `SEND_FEE_BASE_MSAT`       | no       | `10000`           | Base send fee (msat)                        |
| `SEND_FEE_PPM`             | no       | `3000`            | Send fee rate (ppm)                         |
| `RECEIVE_FEE_BASE_MSAT`    | no       | `10000`           | Base receive fee (msat)                     |
| `RECEIVE_FEE_PPM`          | no       | `1000`            | Receive fee rate (ppm)                      |
| `INVOICE_EXPIRY_SECS`      | no       | `86400`           | Expiry of invoices the gateway issues (s)   |
| `CLTV_EXPIRY_DELTA`        | no       | `500`             | Max total CLTV expiry delta on send routes  |

## License

MIT.
