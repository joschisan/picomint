# Picomint Broker Daemon

A broker moves ecash between mints. It is a client of every mint it serves, holding a balance in each, and it funds a payment into one mint against a payment out of another, for a fee. Nothing about a swap has a timeout: the sender's funds are locked to the broker behind a signature of the destination mint, and the destination mint signs for any receive contract it holds. The sender pays the broker's fee on top of what the recipient receives.

The broker is a single container image: `ghcr.io/joschisan/picomint-broker-daemon:main`. Persist `/data` in a volume and set the network every added mint must run on; mints refuse to run on mainnet for now, so that is signet or regtest:

```bash
docker run -d --name picomint-broker-daemon \
    -e DATA_DIR=/data \
    -e NETWORK=regtest \
    -v broker-data:/data \
    ghcr.io/joschisan/picomint-broker-daemon:main
```

The iroh endpoint on `8080` needs no port forward: it punches through NAT and falls back to a relay.

## Accessing the CLI

The `picomint-broker-cli` binary is included in the container and on the `PATH`. Run CLI commands from the host like:

```bash
docker exec picomint-broker-daemon picomint-broker-cli --help
```

The walkthroughs below use the bare `picomint-broker-cli …` form — prefix with `docker exec picomint-broker-daemon` to run them. Every command prints JSON, and its `--help` ends with the JSON Schema of what it prints, every field explained.

A refused request prints one JSON object on stderr, `{"code": ..., "error": ...}`: the code is a stable name to branch on, the error the message to show the operator. The exit code is 1 for a request the daemon refused, 2 for a usage error and 3 when the daemon is unreachable. Every command's `--help` lists the codes it fails with.

One command prints a secret: `mnemonic` prints the seed words every fund derives from. Whatever an agent reads ends up in a model context and a transcript, so the rules for an agent are: run it only when asked, always with the output piped into a file, never read the file, and open it for the operator if asked. Write the words down from that file yourself and delete it. The same rules end the CLI's `--help`. The shell creates that file with its umask, world-readable on most systems, so create it in a subshell with `umask 077` and it is readable by you alone from the first byte.

A first call to confirm everything is wired up:

```bash
picomint-broker-cli info
```

```json
{
  "broker_pk": "picomintthk4cngg1f0rh1sq0mua88km3sht06p5ltviehc2onkocqqgcek0",
  "network": "regtest",
  "fee": { "base": 10000, "ppm": 3000 }
}
```

## Add Mints

The broker serves swaps between any two of the mints it has added. Add one with an invite code (see [Invite Users](../docker-node/README.md#invite-users) for how nodes produce these):

```bash
picomint-broker-cli client add <invite>
```

It prints the mint's id; `list` prints every added mint with its name:

```bash
picomint-broker-cli client list
```

Remove a mint and delete all of its data:

```bash
picomint-broker-cli client remove <mint-id>
```

This is destructive: check for in-flight swaps via `query` first, otherwise you might lose funds.

For clients of a mint to swap out of it through the broker, its nodes also need to add the broker's `broker_pk` (shown by `picomint-broker-cli info`) to their recommended list — see [Configure Brokers](../docker-node/README.md#configure-brokers). Nothing needs to be configured on the mint a swap lands in: the broker funds a receive contract there like any client would.

## Manage Liquidity

Every swap into a mint is paid out of the broker's balance there, and every swap out of a mint pays into the broker's balance there. So the broker needs a balance in every mint it wants to serve swaps into, and it collects balances in the mints it serves swaps out of, which the operator rebalances now and then.

Every command below takes the mint id as its first argument. Commands that move or read funds also name the account, one of `primary`, `secondary`, `tertiary`, `quaternary` or `quinary`. Swaps are always funded from `primary` and paid into `primary`; funds in any other account sit outside the swap pool, which is how an operator keeps a reserve the swap flow can't touch. Amounts carry their denomination, so quote them: `"1000 sat"` or `"0.001 BTC"`.

Check a balance with:

```bash
picomint-broker-cli client balance <mint-id> <account>
```

You can move funds in and out either onchain or as an ecash string.

**Receive Onchain:** generate a mint deposit address and send bitcoin to it. When the transaction confirms the mint issues ecash to the broker.

```bash
picomint-broker-cli client onchain receive <mint-id> <account>
```

The mint sweeps a deposit into its wallet before it credits it, and the miner fee of that sweep comes off the deposit, which is the destination side of what `rebalance` weighs:

```bash
picomint-broker-cli client onchain receive-fee <mint-id>
```

**Send Onchain:** burn ecash in exchange for an onchain transfer to the given address. The mint picks a feerate; check what it will charge first:

```bash
picomint-broker-cli client onchain send-fee <mint-id>
```

Then send:

```bash
picomint-broker-cli client onchain send <mint-id> <account> <address> "<amount>"
```

To empty the account instead, `client onchain send-max <mint-id> <account> <address>` sends everything minus the fee.

Passing `--fee <amount>` overrides the feerate with an exact value; otherwise whatever `send-fee` currently reports is used. The command returns the operation id; the onchain txid lands in the analytics as `onchain_send_success` once the mint has broadcast.

**Send Ecash:** spend part of the mint balance as a base32-encoded ecash string you can hand to another client:

```bash
picomint-broker-cli client ecash send <mint-id> <account> "<amount>"
```

`client ecash send-max <mint-id> <account>` hands out the whole balance as one string.

**Receive Ecash:** reissue an ecash string produced by `client ecash send` (on this broker or any other client) into your balance. Returns the operation id; the reissuance's acceptance shows up in the analytics as `tx_accept`:

```bash
picomint-broker-cli client ecash receive <mint-id> <account> <ecash>
```

## How a Swap Settles

A client asks the broker to swap once it has locked a send contract in its mint: the amount the recipient receives plus the broker's fee, spendable by the broker only with the destination mint's signature over the receive contract. The broker checks the request against the source mint, funds the receive contract in the destination mint out of its balance there, collects the destination mint's signature over it, and claims the send contract with that signature. The signature is also the receipt the client gets back.

One state machine carries a swap from the funding to the claim, and it resumes on restart like every other, so a broker that stops in between picks the swap up where it was. A request the sender repeats, because it lost the response, is answered again without a second funding.

A request the broker's balance in the destination does not cover is refused before anything is recorded, and the sender's next request funds once the balance does. The sender's funds stay locked until then: there is no timeout on a send contract, so a broker that never funds strands them. Keep every destination funded.

## Rebalance

Every swap moves the broker's balance out of the destination mint and into the source, so over time the mints a broker serves swaps into run dry while the ones it serves swaps out of pile up. One command evens them out:

```bash
picomint-broker-cli rebalance
```

It takes the mint with the highest balance and the one with the lowest, and moves the smaller of the source's surplus and the destination's deficit against the mean of every balance, so one of the two lands on the mean and drops out. Run after run, at most one transfer fewer than there are mints brings every balance to the mean. It quotes the transfer onchain first, the source's send fee and the destination's sweep fee, and makes it only if the quote fits the broker's own fee on that amount. The flow that built the imbalance paid at least that much, so a rebalance that passes the test never costs more than it earned. It prints the transfer when it made one, and `null` when it did not:

```json
{
  "source": "8046bcd8c6f1e9a2b3d4c5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081920",
  "destination": "3c1e7a...9f0b",
  "amount": 1250000,
  "operation": "e7b2...41aa"
}
```

The command reads nothing but the balances: a transfer still waiting for its confirmations is invisible to it, and a run within that hour would send again. Run it on a timer no tighter than a couple of hours, or by hand after checking balances. Since the chain fees are flat and the budget grows with the amount, a small imbalance never qualifies; the fee schedule sets how large an imbalance has to get before it is worth moving.

## Restore

If your broker deployment is ever corrupted you can restore your balances from your twelve word mnemonic:

```bash
(umask 077; picomint-broker-cli mnemonic > mnemonic.json)
```

The mnemonic can be used with any Picomint wallet to restore the funds in the mints. Swaps in flight at the time are lost with the database.

## Analytics

The broker mirrors its client's event log into a SQLite database at `{DATA_DIR}/analytics/analytics.sqlite`. The directory is **wiped on every startup** and rebuilt by replaying the event log — analytics are derived, not authoritative, so it's safe to delete and let it rebuild.

The schema is a 1:1 translation of the log: one table per event, named after its kind (`swap_broker_swap`, `swap_broker_success`, `swap_broker_failure`, `tx_create`, `tx_accept`, ...). Every table starts with the same columns — `id` (position in the event log), `ts` (ms since epoch), `mint`, `account`, `operation` — followed by the event's own fields: amounts as integers, msat everywhere except the onchain tables, which are sat, hashes, ids and keys as hex or bech32 text. There are no views; a swap's story is a join on `operation`: `swap_broker_swap` is logged under the destination mint, where the funding lands, and `swap_broker_success` under the source mint, where the claim lands. `query --help` prints every table as the SQL that creates it, with no daemon running.

Query it with read-only SQL through the admin CLI — the daemon runs the query against the live db and returns one JSON object per row, the same shape `sqlite3 --json` prints. Ten most recent swaps with their outcome:

```bash
picomint-broker-cli query \
    "SELECT s.ts, s.mint AS destination, s.amount, s.fee, c.mint AS source, \
            c.id IS NOT NULL AS succeeded, x.id IS NOT NULL AS failed \
     FROM swap_broker_swap s \
     LEFT JOIN swap_broker_success c USING (operation) \
     LEFT JOIN swap_broker_failure x USING (operation) \
     ORDER BY s.ts DESC LIMIT 10"
```

Swaps funded but not yet claimed:

```bash
picomint-broker-cli query \
    "SELECT s.operation, s.ts, s.amount FROM swap_broker_swap s \
     LEFT JOIN swap_broker_success c USING (operation) \
     LEFT JOIN swap_broker_failure x USING (operation) \
     WHERE c.id IS NULL AND x.id IS NULL"
```

## Interfaces

| Port | Purpose                           | Safe to expose? |
|------|-----------------------------------|-----------------|
| 8080 | Public API (iroh / QUIC over UDP) | Yes, not required |

Iroh punches through NAT and falls back to a relay, so `8080` works without a port forward.

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no network exposure, and the daemon keeps it and the data directory owner-only. Reach it with `docker exec picomint-broker-daemon picomint-broker-cli …`.

## Configuration

| Env             | Required | Default        | Description                                                                          |
|-----------------|----------|----------------|--------------------------------------------------------------------------------------|
| `DATA_DIR`      | yes      |                | Directory for the database and the admin socket                                      |
| `NETWORK`       | yes      |                | `testnet`, `signet` or `regtest`; every added mint must run on it, and mainnet mints are refused |
| `API_ADDR`      | no       | `0.0.0.0:8080` | Public API listen address                                                            |
| `FEE_BASE_MSAT` | no       | `10000`        | Base fee on every swap (msat)                                                        |
| `FEE_PPM`       | no       | `3000`         | Fee rate on every swap (ppm)                                                         |
