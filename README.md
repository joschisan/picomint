# Picomint - Alpha

A minimal implementation of a federated Chaumian ecash mint on Bitcoin.

## Deploy Guardian

Download the compose file:

```bash
curl -O https://raw.githubusercontent.com/joschisan/picomint/main/docker-server/docker-compose.yml
```

And then run:

```bash
docker-compose up -d
```

Admin actions including setup either go through the CLI running inside the container:

```bash
docker exec -it picomint-server picomint-server-cli setup status
```

You can also enable the Web UI -- uncomment `UI_ADDR` and `UI_PASSWORD` in `docker-compose.yml` plus the `127.0.0.1:3000:3000` port mapping and restart the container. Never expose the UI to the public internet without TLS - if you dont run on a local machine you can either configure a domain or forward the port over SSH to a port on your local machine:

```bash
ssh -NL 3000:127.0.0.1:3000 <your_server>
```

### Setup Ceremony

Before the federation can start processing transactions, guardians run a one-time setup ceremony. The Web UI walks you through it in a setup wizard; the CLI does the same thing.

Exactly one guardian sets the global federation config and passes `--federation-name` and `--federation-size`; the others pass only their own `<name>`:

```bash
picomint-server-cli setup set-local-params <name> [--federation-name X] [--federation-size N]
```

`set-local-params` returns a setup code. Every guardian then calls `add-peer` once per peer with that peer's setup code:

```bash
picomint-server-cli setup add-peer <setup-code>
```

Once every guardian has added every peer, everyone runs:

```bash
picomint-server-cli setup start-dkg
```

Check your progress with:

```bash
picomint-server-cli setup status
```

### Invite Users

Users join the federation with an invite code and any guardian can create one:

```bash
picomint-server-cli invite
```

The client can use this invite to download and verify the federation config from the guardian that generated it.

### Configure Gateways

The federation maintains an explicit list of recommended Lightning gateways. Any guardian can add a gateway and clients will priorititze gateways by the number of guardians recommending them.

Add a gateway:

```bash
picomint-server-cli module ln gateway add <url>
```

Remove one:

```bash
picomint-server-cli module ln gateway remove <url>
```

List the current recommendations:

```bash
picomint-server-cli module ln gateway list
```

### Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Iroh endpoint                | Yes             |
| 3000 | Web UI (setup + dashboard)   | Localhost only  |

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no
network exposure. Reach it with `docker exec -it picomint-server
picomint-server-cli …`.

### Configuration

| Env                          | Required | Default           | Description                                |
|------------------------------|----------|-------------------|--------------------------------------------|
| `DATA_DIR`                   | yes      |                   | Directory for the redb database file       |
| `BITCOIN_NETWORK`            | yes      | `regtest`         | `bitcoin`, `testnet`, `signet`, `regtest`  |
| `ESPLORA_URL`                | one of   |                   | Esplora HTTP URL, e.g. `https://mempool.space/api` |
| `BITCOIND_URL`               | one of   |                   | Bitcoin Core RPC URL                       |
| `BITCOIND_USERNAME`          | if RPC   |                   | Bitcoin Core RPC user                      |
| `BITCOIND_PASSWORD`          | if RPC   |                   | Bitcoin Core RPC password                  |
| `P2P_ADDR`                   | no       | `0.0.0.0:8080`    | Iroh endpoint listen address               |
| `UI_ADDR`                    | no       |                   | Web UI listen address — unset disables UI  |
| `UI_PASSWORD`                | if UI    |                   | Web UI password, required when `UI_ADDR` is set |

*Either `ESPLORA_URL` or `BITCOIND_URL` must be set, but not both.*

## Deploy Gateway

Download the compose file:

```bash
curl -O https://raw.githubusercontent.com/joschisan/picomint/main/docker-gateway/docker-compose.yml
```

And then run:

```bash
docker-compose up -d
```

Admin actions go through `picomint-gateway-cli`, running inside the container:

```bash
docker exec -it picomint-gateway picomint-gateway-cli info
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
picomint-gateway-cli ldk channel open <pubkey> <host> <channel-size-sats>
```

Running a second outbound channel alongside the LSP's inbound one is worthwhile: with only one channel, outgoing payments can fail once user balances drain toward the counterparty's channel reserve. Monitor channel state with:

```bash
picomint-gateway-cli ldk channel list
```

### Join Federations

The gateway can serve mutliple Federations simultanously. Join one with an invite code (see [Invite Users](#invite-users) above for how guardians produce these):

```bash
picomint-gateway-cli federation join <invite>
```

List joined federations:

```bash
picomint-gateway-cli federation list
```

For the gateway to actually route payments on behalf of a federation, its guardians also need to add the gateway's URL to their recommended list — see [Configure Gateways](#configure-gateways) above.

### Manage Federation Liquidity

Every command below accepts `--id <federation-id>` to target a specific federation. When exactly one federation is joined (the common case) the flag can be omitted and that federation is used.

The gateway holds its own ecash balance in every federation it has joined. Check it with:

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

### Recovery

If your gateway deployment is ever corrupted you can recover your onchain funds and ecash from your twelve word mnemonic:

```bash
picomint-gateway-cli mnemonic
```

The mnemonic can be used with any Bip 39 compatible wallet to recover the onchain funds and with any Picomint wallet to recover the funds in the federations.  **The balance in your open lightning channels is lost.**

### Analytics

The gateway mirrors every gw-module event into a SQLite database at
`{DATA_DIR}/analytics/analytics.sqlite`. The directory is **wiped on every
startup** and rebuilt by replaying the event log — analytics are derived,
not authoritative, so it's safe to delete and let it rebuild.

Inspect the DB with `sqlite3` directly (the gateway container already has
it installed). Pass `-header -column` for human-readable, column-aligned
output — without it `sqlite3` prints unlabeled pipe-delimited rows. See
the ten most recent payments:

```bash
docker exec -it picomint-gateway \
    sqlite3 -header -column /data/analytics/analytics.sqlite \
    "SELECT * FROM payments ORDER BY started_at DESC LIMIT 10;"
```

Breakdown by status:

```bash
docker exec -it picomint-gateway \
    sqlite3 -header -column /data/analytics/analytics.sqlite \
    "SELECT status, COUNT(*) FROM payments GROUP BY status;"
```

Total processed volume per federation, in sats:

```bash
docker exec -it picomint-gateway \
    sqlite3 -header -column /data/analytics/analytics.sqlite \
    "SELECT federation_id, SUM(amount_msat)/1000 AS sats FROM payments WHERE status='success' GROUP BY federation_id;"
```

Each row in `payments` is one incoming or outgoing operation.

| Column          | Type           | Notes                                                                                                    |
|-----------------|----------------|----------------------------------------------------------------------------------------------------------|
| `federation_id` | TEXT           | Hex-encoded federation id                                                                                |
| `operation_id`  | TEXT           | Hex-encoded operation id; unique within `(federation_id, direction)`                                     |
| `direction`     | TEXT           | `incoming` or `outgoing`                                                                                 |
| `status`        | TEXT           | `pending`, `success`, `cancelled` (outgoing only), `failure` (incoming only), `refunded` (incoming only) |
| `started_at`    | INTEGER        | When the operation was initiated (µs since epoch)                                                        |
| `completed_at`  | INTEGER        | NULL while `status = 'pending'`                                                                          |
| `amount_msat`   | INTEGER        | Millisatoshis; NULL on outgoing rows that pay an amountless bolt11 invoice                               |
| `preimage`      | TEXT           | Hex-encoded; NULL unless `status = 'success'`                                                            |

The raw event tables (`send`, `send_success`, `send_cancel`, `receive`,
`receive_success`, `receive_failure`, `receive_refund`) are also queryable
if you need a view more granular than `payments`.

### Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Public API (HTTP)            | Yes             |
| 9735 | LDK Lightning P2P (BOLT)     | Yes             |

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no
network exposure. Reach it with `docker exec -it picomint-gateway
picomint-gateway-cli …`.

### Configuration

| Env                        | Required | Default           | Description                                 |
|----------------------------|----------|-------------------|---------------------------------------------|
| `DATA_DIR`                 | yes      |                   | Directory for redb + LDK node data          |
| `BITCOIN_NETWORK`          | yes      |                   | Bitcoin network the gateway runs on         |
| `ESPLORA_URL`              | one of   |                   | Esplora HTTP URL                            |
| `BITCOIND_URL`             | one of   |                   | Bitcoin Core RPC URL                        |
| `BITCOIND_USERNAME`        | if RPC   |                   | Bitcoin Core RPC user                       |
| `BITCOIND_PASSWORD`        | if RPC   |                   | Bitcoin Core RPC password                   |
| `API_ADDR`                 | no       | `0.0.0.0:8080`    | Public API listen address                   |
| `LDK_ADDR`                 | no       | `0.0.0.0:9735`    | LDK Lightning P2P listen address (BOLT)     |
| `ROUTING_FEE_BASE_MSAT`    | no       | `2000`            | Lightning base routing fee (msat)           |
| `ROUTING_FEE_PPM`          | no       | `3000`            | Lightning routing fee rate (ppm)            |
| `TRANSACTION_FEE_BASE_MSAT`| no       | `2000`            | Federation transaction base fee (msat)      |
| `TRANSACTION_FEE_PPM`      | no       | `3000`            | Federation transaction fee rate (ppm)       |

## License

MIT.
