# Picomint Client Daemon

A headless client for machines: the client library behind an admin socket, with the same analytics mirror the gateway keeps. It exists for load generation, latency measurement and agents, not for people — the app is the client for people. Every command is one client call and returns what the call returns; sends hand back an operation id, and the outcome is read from the analytics through `query`.

The daemon is a single container image: `ghcr.io/joschisan/picomint-client-daemon:main`. Persist `/data` in a volume and set the network every added mint must run on:

```bash
docker run -d --name picomint-client-daemon \
    -e DATA_DIR=/data \
    -e NETWORK=regtest \
    -v client-data:/data \
    ghcr.io/joschisan/picomint-client-daemon:main
```

## Accessing the CLI

The `picomint-client-cli` binary is included in the container and on the `PATH`. Run CLI commands from the host like:

```bash
docker exec picomint-client-daemon picomint-client-cli --help
```

The walkthroughs below use the bare `picomint-client-cli …` form — prefix with `docker exec picomint-client-daemon` to run them. Every command prints JSON.

Every command takes the mint id first and, where funds move, the account next, one of `primary`, `secondary`, `tertiary`, `quaternary` or `quinary`. Amounts carry their denomination, so quote them: `"1000 sat"` or `"0.001 BTC"`.

## Add Mints

The daemon can hold balances in multiple mints. Add one with an invite code (see [Invite Users](../docker-node/README.md#invite-users) for how nodes produce these); joining fetches and verifies the mint's config and scans for any funds a previous client with the same mnemonic left in it:

```bash
picomint-client-cli add <invite>
```

List added mints with their ids, which every other command takes as its first argument:

```bash
picomint-client-cli list
```

```json
{
  "mints": [
    { "mint": "8046bcd8c6f1e9a2b3d4c5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081920", "mint_name": "Bitcoin Beach" }
  ]
}
```

`config <mint>` prints the mint's client config. Remove a mint and delete all of its data:

```bash
picomint-client-cli remove <mint>
```

This is destructive: check for in-flight payments via `query` first, otherwise you might lose funds.

## Balances

The balance of an account, in msat:

```bash
picomint-client-cli balance <mint> <account>
```

```json
{
  "balance_msat": 150000000
}
```

`ecash count <mint> <account>` breaks the balance down into the notes that make it up, keyed by denomination, which is what a load test watches to see the note pool it is drawing from.

## Fund an Account

**Receive Onchain:** generate a mint deposit address and send bitcoin to it. When the transaction confirms the mint issues ecash to the account. The address lands in the analytics as `onchain_receive` when generated, and the claim as `core_tx_accept` under the same operation once the deposit has confirmed:

```bash
picomint-client-cli onchain receive <mint> <account>
```

**Receive Ecash:** reissue an ecash string produced by any client's `ecash send` into the account. Returns the operation id; the reissuance's acceptance shows up in the analytics as `core_tx_accept`:

```bash
picomint-client-cli ecash receive <mint> <account> <ecash>
```

## Move Funds Out

**Send Ecash:** spend part of the balance as a base32-encoded ecash string you can hand to another client:

```bash
picomint-client-cli ecash send <mint> <account> "<amount>"
```

`ecash send-max <mint> <account>` hands out the whole balance as one string.

**Send Onchain:** burn ecash in exchange for an onchain transfer to the given address. The mint picks a feerate; check what it will charge first:

```bash
picomint-client-cli onchain send-fee <mint>
```

Then send:

```bash
picomint-client-cli onchain send <mint> <account> <address> "<amount>"
```

To empty the account instead, `onchain send-max <mint> <account> <address>` sends everything minus the fee. Passing `--fee <amount>` overrides the feerate with an exact value; otherwise whatever `send-fee` currently reports is used. The command returns the operation id; the onchain txid lands in the analytics as `onchain_send_success` once the mint has broadcast.

## Lightning

Lightning payments go through the gateways the mint recommends. The daemon fetches that list and probes every gateway when a mint is added; refresh it after the mint changes its recommendations:

```bash
picomint-client-cli lightning refresh-gateways <mint>
```

**Pay an invoice:** returns the operation id. The outcome lands in the analytics as `lightning_send_success` with the preimage, or `lightning_send_refund` if the gateway could not route it:

```bash
picomint-client-cli lightning send <mint> <account> <invoice>
```

`lightning send-max <mint> <account> <lnurl>` empties the account to an lnurl.

**Create an invoice:** returns the operation id and the invoice. The payment lands in the analytics as `lightning_receive` once the gateway has funded it:

```bash
picomint-client-cli lightning receive <mint> <account> "<amount>"
```

```json
{
  "operation": "3f9c...4a5b",
  "invoice": "lnbc10u1p5..."
}
```

**A reusable lnurl:** an [lnurl daemon](../docker-lnurl) serves invoices on the account's behalf, so the account can be paid while this daemon is offline. Pass its base URL and share the lnurl it returns:

```bash
picomint-client-cli lightning lnurl <mint> <account> https://lnurl.example.com/
```

## Restore

The daemon generates a twelve word mnemonic on first start, and its funds in every mint derive from it:

```bash
picomint-client-cli mnemonic
```

If the deployment is ever lost, the mnemonic restores those funds in any Picomint wallet. The daemon itself cannot be seeded with a mnemonic: its state is the volume, so keep the volume.

## Analytics

The daemon mirrors its event log into a SQLite database at
`{DATA_DIR}/analytics/analytics.sqlite`. The directory is **wiped on every
startup** and rebuilt by replaying the event log — analytics are derived,
not authoritative, so it's safe to delete and let it rebuild.

The schema is a 1:1 translation of the log: one table per event, named
after its source and kind (`core_tx_create`, `core_tx_accept`,
`core_tx_reject`, `ecash_send`, `ecash_receive`, `ecash_success`,
`onchain_send`, `onchain_send_success`, `onchain_receive`,
`lightning_send`, `lightning_send_success`, `lightning_send_refund`,
`lightning_receive`, ...). Every table starts with the same columns — `id`
(position in the event log), `ts` (ms since epoch), `mint`, `account`,
`operation` — followed by the event's own fields: amounts as integers,
msat everywhere except the onchain tables, which are sat, hashes, ids and
keys as hex or bech32 text. There are no views; an operation's story is a
join on `operation`, and the tables of one operation carry the txids that
tie its transactions to their outcome. List them with
`SELECT name FROM sqlite_master WHERE type='table'`.

Query it with read-only SQL through the admin CLI — the daemon runs the
query against the live db and returns one JSON object per row, the same
shape `sqlite3 --json` prints. Mint transaction acceptance latency, the
time between the client submitting a transaction and consensus accepting
it, which is what a latency measurement is after:

```bash
picomint-client-cli query \
    "SELECT c.txid, a.ts - c.ts AS latency_ms \
     FROM core_tx_create c INNER JOIN core_tx_accept a USING (operation, txid) \
     ORDER BY c.ts DESC LIMIT 10"
```

Outgoing Lightning payments with their outcome:

```bash
picomint-client-cli query \
    "SELECT s.ts, s.amount, \
            ss.id IS NOT NULL AS succeeded, sr.id IS NOT NULL AS refunded \
     FROM lightning_send s \
     LEFT JOIN lightning_send_success ss USING (operation) \
     LEFT JOIN lightning_send_refund sr USING (operation) \
     ORDER BY s.ts DESC LIMIT 10"
```

A self-payment loop through a gateway is a shell loop over `lightning receive` and `lightning send`; every transaction it produces shows up in the first query, and every payment outcome in the second.

## Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Iroh endpoint (QUIC over UDP) | Yes, not required |

The daemon only dials out through its endpoint, to mints and gateways; nothing has to reach it. The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no network exposure. Reach it with `docker exec picomint-client-daemon picomint-client-cli …`.

## Configuration

| Env                        | Required | Default           | Description                                 |
|----------------------------|----------|-------------------|---------------------------------------------|
| `DATA_DIR`                 | yes      |                   | Directory for the database and analytics    |
| `NETWORK`                  | no       | `bitcoin`         | `bitcoin`, `testnet`, `signet`, `regtest`; every added mint must run on it |
| `API_ADDR`                 | no       | `0.0.0.0:8080`    | Iroh endpoint listen address                |
