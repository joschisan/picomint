# Picomint Client Daemon

A headless client for machines: the client library behind an admin socket, with the same analytics mirror the gateway keeps. It exists for load generation, latency measurement and agents, not for people — the app is the client for people. Every command is one client call and returns what the call returns; sends hand back an operation id, and the outcome is read from the analytics through `query`.

The daemon is a single container image: `ghcr.io/joschisan/picomint-client-daemon:main`. Persist `/data` in a volume and set the network every added mint must run on; mints refuse to run on mainnet for now, so that is signet or regtest:

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

The walkthroughs below use the bare `picomint-client-cli …` form — prefix with `docker exec picomint-client-daemon` to run them. Every command prints JSON, and its `--help` ends with the JSON Schema of what it prints, every field explained.

A refused request prints one JSON object on stderr, `{"code": ..., "error": ...}`: the code is a stable name to branch on, the error the message to show the operator. The exit code is 1 for a request the daemon refused, 2 for a usage error and 3 when the daemon is unreachable. Every command's `--help` lists the codes it fails with.

One command prints a secret: `mnemonic` prints the seed words every fund derives from. Whatever an agent reads ends up in a model context and a transcript, so the rules for an agent are: run it only when asked, always with the output piped into a file, never read the file, and open it for the operator if asked. Write the words down from that file yourself and delete it. The same rules end the CLI's `--help`. The shell creates that file with its umask, world-readable on most systems, so create it in a subshell with `umask 077` and it is readable by you alone from the first byte.

Every command that acts on one mint takes the mint id first and, where funds move, the account next, one of `primary`, `secondary`, `tertiary`, `quaternary` or `quinary`. Amounts carry their denomination, so quote them: `"1000 sat"` or `"0.001 BTC"`.

## Add Mints

The daemon can hold balances in multiple mints. Add one with an invite code (see [Invite Users](../docker-node/README.md#invite-users) for how nodes produce these); joining fetches and verifies the mint's config and scans for any funds a previous client with the same mnemonic left in it:

```bash
picomint-client-cli add <invite>
```

It prints the mint's id, which every per-mint command takes first; `list` prints every added mint with its name:

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

`config <mint>` prints the mint's client config. A mint winds down by announcing an expiry date, optionally with a successor mint to move funds to; `expiry <mint>` fetches that announcement fresh from the nodes, so an agent holding funds in a mint should check it now and then:

```bash
picomint-client-cli expiry <mint>
```

```json
{
  "expiry": {
    "timestamp": 1814313600,
    "successor": "picominttekvo2jfmj51q1gmatg81ome1hod4fur..."
  }
}
```

The timestamp is midnight UTC of the day the mint winds down, in unix seconds, and the successor is the invite code of the mint to move funds to, absent when there is none; `expiry` is `null` while the mint announces nothing. Remove a mint and delete all of its data:

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

`ecash count <mint> <account>` breaks the balance down into the notes that make it up, keyed by the denomination's exponent, which is what a load test watches to see the note pool it is drawing from.

## Fund an Account

**Receive Onchain:** generate a mint deposit address and send bitcoin to it. When the transaction confirms the mint issues ecash to the account. Nothing is logged for the address itself; once the deposit has six confirmations the claim lands in the analytics as `onchain_receive`, and its acceptance as `tx_accept` under the same operation:

```bash
picomint-client-cli onchain receive <mint> <account>
```

The mint sweeps a deposit into its wallet before it credits it, and the miner fee of that sweep comes off the deposit; a deposit worth no more than the fee is not claimed. Check it before sending small amounts:

```bash
picomint-client-cli onchain receive-fee <mint>
```

**Receive Ecash:** reissue an ecash string produced by any client's `ecash send` into the account. Returns the operation id; the reissuance's acceptance shows up in the analytics as `tx_accept`:

```bash
picomint-client-cli ecash receive <mint> <account> <ecash>
```

## Move Funds Out

**Send Ecash:** spend part of the balance as a base32-encoded ecash string you can hand to another client:

```bash
picomint-client-cli ecash send <mint> <account> "<amount>"
```

`ecash send-max <mint> <account>` hands out every note the account holds as one string. Nothing is spent at the mint, so the string is worth exactly `balance` and there is no separate amount to ask for.

**Send Onchain:** burn ecash in exchange for an onchain transfer to the given address. The mint picks a feerate; check what it will charge first:

```bash
picomint-client-cli onchain send-fee <mint>
```

Then send:

```bash
picomint-client-cli onchain send <mint> <account> <address> "<amount>"
```

Passing `--fee <amount>` overrides the feerate with an exact value; otherwise whatever `send-fee` currently reports is used. The command returns the operation id; the onchain txid lands in the analytics as `onchain_send_success` once the mint has broadcast.

To empty the account instead, `onchain send-max <mint> <account> <address>` spends every note it holds in one transaction. Unlike `ecash send-max`, that goes through the mint, so fees come off: the destination receives the largest whole-sat amount that fits once the miner fee `send-fee` quotes, the mint's per-output fee and the mint's per-input fee on each note spent are covered, and the sub-sat remainder stays with the mint. `onchain send-max-amount <mint> <account>` computes that amount, in sat, without sending, so you can decide before you commit:

```bash
picomint-client-cli onchain send-max-amount <mint> <account>
```

```json
{
  "amount_sat": 149210
}
```

## Lightning

Lightning payments go through the gateways the mint recommends. The daemon fetches that list and probes every gateway when a mint is added; list the ones that answered, keyed by their `gateway_pk`, with the fees each charges:

```bash
picomint-client-cli lightning gateway list <mint>
```

```json
{
  "gateways": {
    "picomintd2g4...c9d1": {
      "module_public_key": "8f3a...b2e7",
      "send_fee": { "base": 10000, "ppm": 3000 },
      "receive_fee": { "base": 10000, "ppm": 1000 },
      "expiry_delta": 500
    }
  }
}
```

Fees are in msat plus parts per million of the amount. Every send and receive names the gateway it goes through, so the fee you read here is the fee you pay: the list only changes when you refresh it, which re-fetches the mint's recommendations and re-probes every gateway:

```bash
picomint-client-cli lightning gateway refresh <mint>
```

**Pay an invoice:** returns the operation id. The outcome lands in the analytics as `lightning_send_success` with the preimage, or `lightning_send_refund` if the gateway could not route it:

```bash
picomint-client-cli lightning send <mint> <account> <gateway> <invoice>
```

`lightning send-max <mint> <account> <gateway> <lnurl>` empties the account to an lnurl: it resolves the lnurl, requests one invoice for the maximum and pays it. The maximum follows the same rule as `onchain send-max`, with the gateway's `send_fee` from `lightning gateway list` in place of the miner fee: the invoice is for the largest whole-sat amount that fits once that fee on it, the mint's per-output fee and the mint's per-input fee on each note spent are covered. It depends on the gateway, so `lightning send-max-amount <mint> <account> <gateway>` takes one and computes the invoice amount, in msat, without paying:

```bash
picomint-client-cli lightning send-max-amount <mint> <account> <gateway>
```

```json
{
  "amount_msat": 149210000
}
```

**Create an invoice:** returns the invoice. The payment lands in the analytics as `lightning_receive` once the gateway has funded it, under the operation derived from the invoice's payment hash:

```bash
picomint-client-cli lightning receive <mint> <account> <gateway> "<amount>"
```

```json
{
  "invoice": "lnbc10u1p5..."
}
```

**A reusable lnurl:** an lnurl daemon serves invoices on the account's behalf, so the account can be paid while this daemon is offline. It is a hosted service, not something you run: pass its base URL and share the lnurl it returns:

```bash
picomint-client-cli lightning lnurl <mint> <account> https://lnurl.example.com/
```

## Swap

A swap address receives ecash from any mint: the sender's own mint pays it directly, any other mint pays it through a broker the sender's mint recommends. The address is static, so hand it out once:

```bash
picomint-client-cli swap receive <mint> <account>
```

```json
{
  "address": "picomint3g8k...q2mz"
}
```

Payments to it land in the analytics as `swap_receive` and need no action here: the daemon scans the mint's receive contracts and claims its own.

**Pay an address in this mint:** the address names its mint, so compare it with the one you pay from. An address of the same mint is paid as one transaction, and `tx_accept` is its outcome:

```bash
picomint-client-cli swap send-direct <mint> <account> <address> "<amount>"
```

`swap send-max-direct <mint> <account> <address>` empties the account to it: the largest whole-sat amount that fits once the mint's per-output fee and its per-input fee on each note spent are covered, which `swap send-max-amount-direct <mint> <account>` computes, in msat, without sending.

**Pay an address through a broker:** the way to reach another mint, though a broker will also route back into the mint you pay from. The outcome lands as `swap_send_success` with the destination mint's attestation once the broker has funded the receive contract there:

```bash
picomint-client-cli swap send <mint> <account> <broker> <address> "<amount>"
```

`swap send-max <mint> <account> <broker> <address>` empties the account through the broker, with the broker's fee on the amount covered as well as the mint's fees; `swap send-max-amount <mint> <account> <broker>` computes that amount, in msat, without sending:

```bash
picomint-client-cli swap send-max-amount <mint> <account> <broker>
```

```json
{
  "amount_msat": 99000000
}
```

The broker's fee is charged on top of the amount, and there is no refund path: the funds are locked to the broker until the destination mint attests the receive contract, so pick a broker you trust to fund it. The brokers the mint recommends, probed for their fees, keyed by their `broker_pk`:

```bash
picomint-client-cli swap broker list <mint>
```

```json
{
  "brokers": {
    "picomintd2g4...c9d1": {
      "claim_pk": "8f3a...b2e7",
      "fee": { "base": 10000, "ppm": 3000 }
    }
  }
}
```

The list only changes when you refresh it, which re-fetches the mint's recommendations and re-probes every broker:

```bash
picomint-client-cli swap broker refresh <mint>
```

## Restore

The daemon generates a twelve word mnemonic on first start, and its funds in every mint derive from it:

```bash
(umask 077; picomint-client-cli mnemonic > mnemonic.json)
```

If the deployment is ever lost, the mnemonic restores those funds in any Picomint wallet. The daemon itself cannot be seeded with a mnemonic: its state is the volume, so keep the volume.

## Analytics

The daemon mirrors its event log into a SQLite database at
`{DATA_DIR}/analytics/analytics.sqlite`. The directory is **wiped on every
startup** and rebuilt by replaying the event log — analytics are derived,
not authoritative, so it's safe to delete and let it rebuild.

The schema is a 1:1 translation of the log: one table per event, named
after its kind (`tx_create`, `tx_accept`, `tx_reject`, `ecash_send`,
`ecash_receive`, `ecash_success`,
`onchain_send`, `onchain_send_success`, `onchain_receive`,
`lightning_send`, `lightning_send_success`, `lightning_send_refund`,
`lightning_receive`, ...). Every table starts with the same columns — `id`
(position in the event log), `ts` (ms since epoch), `mint`, `account`,
`operation` — followed by the event's own fields: amounts as integers,
msat everywhere except the onchain tables, which are sat, hashes, ids and
keys as hex or bech32 text. There are no views; an operation's story is a
join on `operation`, and the tables of one operation carry the txids that
tie its transactions to their outcome. `query --help` prints every table
as the SQL that creates it, with no daemon running.

Query it with read-only SQL through the admin CLI — the daemon runs the
query against the live db and returns one JSON object per row, the same
shape `sqlite3 --json` prints. Mint transaction acceptance latency, the
time between the client submitting a transaction and consensus accepting
it, which is what a latency measurement is after:

```bash
picomint-client-cli query \
    "SELECT c.txid, a.ts - c.ts AS latency_ms \
     FROM tx_create c INNER JOIN tx_accept a USING (operation, txid) \
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

The daemon only dials out through its endpoint, to mints and gateways; nothing has to reach it. The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no network exposure, and the daemon keeps it and the data directory owner-only. Reach it with `docker exec picomint-client-daemon picomint-client-cli …`.

## Configuration

| Env                        | Required | Default           | Description                                 |
|----------------------------|----------|-------------------|---------------------------------------------|
| `DATA_DIR`                 | yes      |                   | Directory for the database and analytics    |
| `NETWORK`                  | yes      |                   | `testnet`, `signet` or `regtest`; every added mint must run on it, and mainnet mints are refused |
| `API_ADDR`                 | no       | `0.0.0.0:8080`    | Iroh endpoint listen address                |
