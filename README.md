# Picomint - Alpha

A minimal implementation of a federated Chaumian ecash mint on Bitcoin.

## Deploy Guardian

Guardians run on a fresh **Ubuntu 26.04 LTS desktop** (amd64) with a screen and keyboard:

```bash
curl -fsSL https://raw.githubusercontent.com/joschisan/picomint/main/bootstrap.sh | bash
```

The installer is fully self-contained — the compose file and updater are embedded in the script and written to `~/picomint-guardian-daemon`. It installs Docker (if missing), brings up the guardian + a bundled bitcoind + a log viewer, opens the Web UI at <http://127.0.0.1:3000>, pins Dashboard / Logs / Update shortcuts to the dock, and installs Signal Desktop for exchanging setup codes with co-guardians. It is safe to re-run at any time; guardian state lives in Docker volumes a re-run never touches. CI runs the bootstrap end-to-end on GitHub Actions' `ubuntu-26.04` runner.

### Bitcoin Backend

The guardian runs as a lightweight daemon on top of a local **unpruned** Bitcoin Core node. The bundled compose starts one for you alongside the guardian. Any machine that can comfortably run Bitcoin Core can run the picomint guardian on top — picomint's own resource footprint is negligible compared to Core's.

Pruning is not supported: a halted federation must be able to resume from blocks that may pre-date a rolling prune window.

Initial block download pulls the full chain over the network, so expect the first boot on mainnet to take a long time and several hundred GB of bandwidth and disk. The guardian will sit idle until bitcoind catches up.

### Accessing the CLI

The `picomint-guardian-cli` binary is included in the container and on the `PATH`. Run CLI commands from the host like:

```bash
sudo docker exec picomint-guardian-daemon picomint-guardian-cli --help
```

The walkthroughs below use the bare `picomint-guardian-cli …` form — prefix with `sudo docker exec picomint-guardian-daemon` to run them.

### Setup Ceremony

Before the federation can start processing transactions, guardians run a one-time setup ceremony. The Web UI walks you through it in a setup wizard; the CLI does the same thing.

Exactly one guardian sets the global federation config and passes `--federation-name` and `--federation-size`; the others pass only their own `<name>`:

```bash
picomint-guardian-cli setup set-local-params <name> [--federation-name X] [--federation-size N]
```

`set-local-params` returns a setup code. Every guardian then calls `add-peer` once per peer with that peer's setup code:

```bash
picomint-guardian-cli setup add-peer <setup-code>
```

Once every guardian has added every peer, everyone runs:

```bash
picomint-guardian-cli setup start-dkg
```

Check your progress with:

```bash
picomint-guardian-cli setup status
```

### Invite Users

Users add the federation with an invite code and any guardian can create one:

```bash
picomint-guardian-cli invite
```

The client can use this invite to download and verify the federation config from the guardian that generated it.

### Configure Gateways

The federation maintains an explicit list of recommended Lightning gateways. Any guardian can add a gateway and clients will priorititze gateways by the number of guardians recommending them.

Add a gateway:

```bash
picomint-guardian-cli module ln gateway add <url>
```

Remove one:

```bash
picomint-guardian-cli module ln gateway remove <url>
```

List the current recommendations:

```bash
picomint-guardian-cli module ln gateway list
```

### Backup

Once the setup ceremony completes, save your guardian's config to a file on
your local machine and stash it somewhere safe (encrypted backup, password
manager, paper printout):

```bash
picomint-guardian-cli config > config.json
```

This single file is the only state you need to keep. It contains your
guardian's secret keys plus the federation's consensus config. The live
`database.redb` is operational state (BFT sessions, block sync) which is
reconstructed from peers when a restored guardian rejoins.

If your deployment is ever lost, copy the backup back into a fresh container:

```bash
sudo docker cp config.json picomint-guardian-daemon:/tmp/config.json
```

And run `setup restore`:

```bash
picomint-guardian-cli setup restore /tmp/config.json
```

### Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Iroh endpoint                | Yes             |
| 3000 | Web UI (setup + dashboard)   | Localhost only  |

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no
network exposure. Reach it with `sudo docker exec picomint-guardian-daemon
picomint-guardian-cli …`.

### Configuration

| Env                          | Required | Default           | Description                                |
|------------------------------|----------|-------------------|--------------------------------------------|
| `DATA_DIR`                   | yes      |                   | Directory for the redb database file       |
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

To route payments on behalf of federations the gateway needs Lightning channels — specifically inbound liquidity, since a fresh node cannot receive payments. The usual approach is to buy an inbound channel from a Lightning Service Provider (LSP) such as [LN Big](https://lnbig.com). LSPs will ask for the node's `public_key` from `info` above and may require you to connect to them before they open the channel:

```bash
picomint-gateway-cli ldk peer connect <lsp-pubkey> <lsp-host>
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

### Add Federations

The gateway can serve mutliple Federations simultanously. Add one with an invite code (see [Invite Users](#invite-users) above for how guardians produce these):

```bash
picomint-gateway-cli federation add <invite>
```

List added federations:

```bash
picomint-gateway-cli federation list
```

For the gateway to actually route payments on behalf of a federation, its guardians also need to add the gateway's URL to their recommended list — see [Configure Gateways](#configure-gateways) above.

### Manage Federation Liquidity

Every command below accepts `--id <federation-id>` to target a specific federation. When exactly one federation is added (the common case) the flag can be omitted and that federation is used.

The gateway holds its own ecash balance in every federation it has added. Check it with:

```bash
picomint-gateway-cli federation balance
```

You can move funds in and out either onchain or as an ecash string.

**Receive Onchain:** generate a federation deposit address and send bitcoin to it. When the transaction confirms the federation mints ecash to the gateway.

```bash
picomint-gateway-cli federation module wallet receive
```

**Send Onchain:** burn ecash in exchange for an onchain transfer to the given address. The federation picks a feerate; check what it will charge first:

```bash
picomint-gateway-cli federation module wallet send-fee
```

Then send:

```bash
picomint-gateway-cli federation module wallet send <address> <amount>
```

Passing `--fee <amount>` overrides the feerate with an exact value; otherwise whatever `send-fee` currently reports is used.

**Send Ecash:** spend part of the federation balance as a base32-encoded ecash string you can hand to another client:

```bash
picomint-gateway-cli federation module mint send <amount>
```

**Receive Ecash:** reissue an ecash string produced by `mint send` (on this gateway or any other client) into your balance:

```bash
picomint-gateway-cli federation module mint receive <ecash>
```

### Restore

If your gateway deployment is ever corrupted you can restore your onchain funds and ecash from your twelve word mnemonic:

```bash
picomint-gateway-cli mnemonic
```

The mnemonic can be used with any Bip 39 compatible wallet to restore the onchain funds and with any Picomint wallet to restore the funds in the federations.  **The balance in your open lightning channels is lost.**

### Analytics

The gateway mirrors every gw-module event into a SQLite database at
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
picomint-gateway-cli analytics \
    "SELECT * FROM outgoing_payments ORDER BY started_at DESC LIMIT 10"
```

Status breakdown for outgoing:

```bash
picomint-gateway-cli analytics \
    "SELECT status, COUNT(*) AS n FROM outgoing_payments GROUP BY status"
```

Total outgoing volume per federation, in sat:

```bash
picomint-gateway-cli analytics \
    "SELECT federation, SUM(amount_msat)/1000 AS sat \
     FROM outgoing_payments WHERE status='success' GROUP BY federation"
```

**Columns common to both views:**

| Column           | Type    | Notes                                                                  |
|------------------|---------|------------------------------------------------------------------------|
| `federation`     | TEXT    | Hex-encoded federation id                                              |
| `operation`      | TEXT    | Hex-encoded operation id; unique within the view                       |
| `status`         | TEXT    | See below                                                              |
| `started_at`     | INTEGER | When the operation was initiated (ms since epoch)                      |
| `completed_at`   | INTEGER | NULL while `status = 'pending'`                                        |
| `amount_msat`    | INTEGER | Payment amount, msat                                                   |
| `gw_fee_msat`    | INTEGER | The gateway's fee cut                                                  |
| `tx_fee_msat`    | INTEGER | Federation consensus tx fee (NULL until the tx lands)                  |
| `tx_remint_msat` | INTEGER | Federation tx remint amount                                            |
| `tx_txid`        | TEXT    | Federation tx id (NULL until the tx lands)                             |
| `preimage`       | TEXT    | Hex-encoded; NULL unless `status = 'success'`                          |

**Additional columns on `outgoing_payments`:**

| Column             | Type    | Notes                                                                  |
|--------------------|---------|------------------------------------------------------------------------|
| `gw_fee_kept_msat` | INTEGER | `gw_fee_msat` less the realized LN routing cost (NULL while pending; 0 if cancelled) |

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
| `DATA_DIR`                 | yes      |                   | Directory for redb + LDK node data          |
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
