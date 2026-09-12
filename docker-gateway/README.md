# Picomint Gateway Daemon


The gateway is a single container image: `ghcr.io/joschisan/picomint-gateway-daemon:main`. Set it up with Docker however you prefer — persist `/data` in a volume, publish the public API port `8080` (iroh — QUIC over UDP) and the LDK Lightning P2P port `9735`, and configure it through the environment variables documented in [Configuration](#configuration) below.

## Accessing the CLI

The `picomint-gateway-cli` binary is included in the container and on the `PATH`. Run CLI commands from the host like:

```bash
docker exec picomint-gateway-daemon picomint-gateway-cli --help
```

The walkthroughs below use the bare `picomint-gateway-cli …` form — prefix with `docker exec picomint-gateway-daemon` to run them.

One command prints a secret, and whatever an agent reads ends up in a model context and a transcript: `mnemonic` prints the seed words every fund derives from. Run it once, to write them down, and tell your agent not to run it unprompted.

A first call to confirm everything is wired up:

```bash
picomint-gateway-cli info
```

Your info will look like

```json
{
  "lightning_pk": "02abfe4a99f1ed8f67c1f07e5d47f3ab3d2e9c5b8a1c8e7f2a6d4b7e9c1f5a3e8d",
  "gateway_pk": "d2g4h6j8k0m1n3p5q7r9s0t2v4w6x8y1z3a5b7c9d1e3f5g7h9j1",
  "alias": "picomint-gateway-daemon",
  "network": "bitcoin",
  "block_height": 842195,
  "synced_to_chain": true
}
```

## Open Channels

To route payments on behalf of mints the gateway needs Lightning channels — specifically inbound liquidity, since a fresh node cannot receive payments. The usual approach is to buy an inbound channel from a Lightning Service Provider (LSP) such as [LN Big](https://lnbig.com). LSPs will ask for the node's `lightning_pk` from `info` above and may require you to connect to them before they open the channel:

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

## Add Mints

The gateway can serve multiple mints simultaneously. Add one with an invite code (see [Invite Users](../docker-node/README.md#invite-users) for how nodes produce these):

```bash
picomint-gateway-cli client add <invite>
```

List added mints:

```bash
picomint-gateway-cli client list
```

Remove a mint and delete all of its data:

```bash
picomint-gateway-cli client remove <mint-id>
```

This is destructive: check for in-flight payments via `query` first, otherwise you might lose funds.

For the gateway to actually route payments on behalf of a mint, its nodes also need to add the gateway's `gateway_pk` (shown by `picomint-gateway-cli info`) to their recommended list — see [Configure Gateways](../docker-node/README.md#configure-gateways).

## Manage Mint Liquidity

Every command below takes the mint id as its first argument. Commands that move or read funds also name the account, one of `primary`, `secondary`, `tertiary`, `quaternary` or `quinary`. Payments are always routed from `primary`; funds in any other account sit outside the routing pool, which is how an operator keeps a reserve the payment flow can't touch.

The gateway holds its own ecash balance in every mint it has added. Check it with:

```bash
picomint-gateway-cli client balance <mint-id> <account>
```

You can move funds in and out either onchain or as an ecash string.

**Receive Onchain:** generate a mint deposit address and send bitcoin to it. When the transaction confirms the mint issues ecash to the gateway.

```bash
picomint-gateway-cli client onchain receive <mint-id> <account>
```

**Send Onchain:** burn ecash in exchange for an onchain transfer to the given address. The mint picks a feerate; check what it will charge first:

```bash
picomint-gateway-cli client onchain send-fee <mint-id>
```

Then send:

```bash
picomint-gateway-cli client onchain send <mint-id> <account> <address> <amount>
```

To empty the account instead, `client onchain send-max <mint-id> <account> <address>` sends everything minus the fee.

Passing `--fee <amount>` overrides the feerate with an exact value; otherwise whatever `send-fee` currently reports is used. The command returns the operation id; the onchain txid lands in the analytics as `onchain_send_success` once the mint has broadcast.

**Send Ecash:** spend part of the mint balance as a base32-encoded ecash string you can hand to another client:

```bash
picomint-gateway-cli client ecash send <mint-id> <account> <amount>
```

`client ecash send-max <mint-id> <account>` hands out the whole balance as one string.

**Receive Ecash:** reissue an ecash string produced by `client ecash send` (on this gateway or any other client) into your balance. Returns the operation id; the reissuance's acceptance shows up in the analytics as `core_tx_accept`:

```bash
picomint-gateway-cli client ecash receive <mint-id> <account> <ecash>
```

## Restore

If your gateway deployment is ever corrupted you can restore your onchain funds and ecash from your twelve word mnemonic:

```bash
picomint-gateway-cli mnemonic
```

The mnemonic can be used with any Bip 39 compatible wallet to restore the onchain funds and with any Picomint wallet to restore the funds in the mints.  **The balance in your open lightning channels is lost.**

## Analytics

The gateway mirrors its client's event log into a SQLite database at
`{DATA_DIR}/analytics/analytics.sqlite`. The directory is **wiped on every
startup** and rebuilt by replaying the event log — analytics are derived,
not authoritative, so it's safe to delete and let it rebuild.

The schema is a 1:1 translation of the log: one table per event, named
after its source and kind (`gateway_send`, `gateway_send_success`,
`core_tx_create`, `core_tx_accept`, `ecash_success`, ...). Every table
starts with the same columns — `id` (position in the event log), `ts`
(ms since epoch), `mint`, `account`, `operation` — followed by the
event's own fields: amounts as integers, msat everywhere except the
onchain tables, which are sat,
hashes, ids and keys as hex or bech32 text. There are no views; an
operation's story is a join on `operation`, and the tables of one
operation carry the txids that tie its transactions to their outcome.
List them with `SELECT name FROM sqlite_master WHERE type='table'`.

Query it with read-only SQL through the admin CLI — the daemon runs the
query against the live db and returns one JSON object per row, the same
shape `sqlite3 --json` prints. Ten most recent outgoing payments with
their outcome:

```bash
picomint-gateway-cli query \
    "SELECT s.ts, s.amount, s.fee, \
            ss.preimage IS NOT NULL AS succeeded, sc.id IS NOT NULL AS cancelled \
     FROM gateway_send s \
     LEFT JOIN gateway_send_success ss USING (operation) \
     LEFT JOIN gateway_send_cancel sc USING (operation) \
     ORDER BY s.ts DESC LIMIT 10"
```

Successful outgoing volume per mint, in sat:

```bash
picomint-gateway-cli query \
    "SELECT s.mint, SUM(s.amount)/1000 AS sat \
     FROM gateway_send s INNER JOIN gateway_send_success USING (operation) \
     GROUP BY s.mint"
```

Mint transaction acceptance latency, the time between the gateway
submitting a transaction and consensus accepting it:

```bash
picomint-gateway-cli query \
    "SELECT c.txid, a.ts - c.ts AS latency_ms \
     FROM core_tx_create c INNER JOIN core_tx_accept a USING (operation, txid) \
     ORDER BY c.ts DESC LIMIT 10"
```

Incoming payments still waiting on their claim:

```bash
picomint-gateway-cli query \
    "SELECT r.operation, r.ts, r.amount FROM gateway_receive r \
     LEFT JOIN gateway_receive_success rs USING (operation) \
     LEFT JOIN gateway_receive_refund rr USING (operation) \
     LEFT JOIN gateway_receive_failure rf USING (operation) \
     WHERE rs.id IS NULL AND rr.id IS NULL AND rf.id IS NULL"
```

## Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Public API (iroh / QUIC over UDP) | Yes |
| 9735 | LDK Lightning P2P (BOLT)     | Yes             |

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no
network exposure. Reach it with `docker exec picomint-gateway-daemon
picomint-gateway-cli …`.

## Configuration

| Env                        | Required | Default           | Description                                 |
|----------------------------|----------|-------------------|---------------------------------------------|
| `DATA_DIR`                 | yes      |                   | Directory for the database + LDK node data  |
| `NETWORK`                  | no       | `bitcoin`         | `bitcoin`, `testnet`, `signet`, `regtest`   |
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
