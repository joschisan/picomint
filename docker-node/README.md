# Picomint Node Daemon


Nodes run on a fresh **Ubuntu 26.04 LTS desktop** (amd64) with a screen and keyboard. The node has no web UI: everything from the setup ceremony on happens through the admin CLI, and this document is the complete manual for it, written so that an operator, or an agent working for one, can run every step.

## Install

Install Docker and let your account use it, then log out and back in so the group takes effect:

```bash
sudo apt update && sudo apt install -y docker.io docker-compose-v2
sudo usermod -aG docker $USER
```

Download [`docker-compose.yml`](docker-compose.yml), which runs the node and Bitcoin Core side by side, then pull and start:

```bash
curl -fsSL --create-dirs -o ~/picomint/docker-compose.yml https://raw.githubusercontent.com/joschisan/picomint/main/docker-node/docker-compose.yml
docker compose -f ~/picomint/docker-compose.yml pull
docker compose -f ~/picomint/docker-compose.yml up -d
```

Both containers restart with the machine. Node state lives in Docker volumes an update never touches; updating is the same `pull` and `up -d` again. Follow the node's log with:

```bash
docker compose -f ~/picomint/docker-compose.yml logs --tail 200 -f picomint-node-daemon
```

## Bitcoin Backend

The node runs as a lightweight daemon on top of a local Bitcoin Core node. The bundled compose starts one for you alongside the node. Any machine that can comfortably run Bitcoin Core can run the picomint node on top — picomint's own resource footprint is negligible compared to Core's.

A pruned node works, under one rule: the mint's block height, the one `status` shows, has to stay inside the prune window. The mint reads blocks from that height onward and never anything older, so the prune window is what limits the longest outage the mint can recover from; a backend whose window has moved past the mint's height has to reindex. Be conservative and size the window for 30 days: `-prune=20000` keeps about that much of mainnet, and is what the bundled compose sets. Remove the line to run a full node.

Initial block download pulls the full chain over the network either way, so expect the first boot on mainnet to take a long time and several hundred GB of bandwidth. The node will sit idle until bitcoind catches up.

For now the node refuses to run a mint on mainnet: `setup confirm` fails with that error when bitcoind runs mainnet, so point it at signet or regtest until this is no longer experimental.

## Accessing the CLI

The `picomint-node-cli` binary is included in the container and on the `PATH`. Run CLI commands from the host like:

```bash
docker exec picomint-node-daemon picomint-node-cli --help
```

The walkthroughs below use the bare `picomint-node-cli …` form — prefix with `docker exec picomint-node-daemon` to run them. Every command prints JSON.

Two commands print secrets, and whatever an agent reads ends up in a model context and a transcript. `backup` prints the node's private keys: always pipe it into a file, never to the terminal. `onchain sweep` prints a share of the wallet key: run it only once the mint has expired, and never before. Tell your agent not to run either unprompted.

## Status

`status` is the first thing to run against any node. It reports which phase the node is in and what matters in that phase:

```bash
picomint-node-cli status
```

- `Setup`: the node's own setup code once `setup init` has run, the mint name and size once any setup code has carried them, and the nodes added so far.
- `Dkg`: key generation is running; the setup code, for nodes that still need it.
- `Consensus`: the mint is up. Mint name and id, network, this node's id and name, consensus version, session count, block height, every node's connection and the bitcoind backend.

On a running mint it looks like this:

```json
{
  "phase": "Consensus",
  "mint_name": "Bitcoin Beach",
  "mint_id": "8046bcd8c6f1e9a2b3d4c5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f7081920",
  "network": "bitcoin",
  "node_id": 2,
  "node_name": "carol",
  "consensus_version": { "major": 1, "minor": 0 },
  "session_count": 48213,
  "block_height": 912340,
  "nodes": [
    { "id": 0, "name": "alice", "connected": true, "transport": "direct", "remote_addr": "203.0.113.7:8080", "rtt_ms": 41 },
    { "id": 1, "name": "bob", "connected": true, "transport": "relay", "remote_addr": "relay.n0.iroh.network", "rtt_ms": 118 },
    { "id": 3, "name": "dave", "connected": false, "transport": null, "remote_addr": null, "rtt_ms": null }
  ],
  "bitcoin": { "network": "bitcoin", "block_height": 912341, "fee_rate_sat_per_vb": 4, "sync_progress": 1.0 }
}
```

The mint wallet has its own status: the value in custody, the transaction holding the wallet and how many mint transactions preceded it, and the consensus fee rate.

```bash
picomint-node-cli onchain status
```

```json
{
  "total_value_sat": 183500000,
  "tx_tip": "6f1e...c3a9",
  "tx_count": 1287,
  "feerate_sat_per_vb": 4
}
```

The mint transactions still waiting for confirmation, and the wallet's whole transaction history, are their own commands:

```bash
picomint-node-cli onchain pending
picomint-node-cli onchain history
```

Every other command belongs to exactly one phase; called in the wrong one it says so and points back here.

## Setup Ceremony

Before the mint can start processing transactions, nodes run a one-time setup ceremony.

Exactly one node sets the global mint config and passes `--mint-name` and `--mint-size`; the others pass only their own `<name>`:

```bash
picomint-node-cli setup init <name> [--mint-name X] [--mint-size N]
```

`init` returns a setup code. Every node then adds every other node's setup code:

```bash
picomint-node-cli setup add <setup-code>
```

`status` shows which nodes have been added so far. If a code was pasted wrong, `setup reset` forgets every added code so you can start collecting them again. Once every node has added every node, everyone confirms the node set:

```bash
picomint-node-cli setup confirm
```

Key generation starts once every node has confirmed. `status` reports `Dkg` while the keys are generated and `Consensus` once the mint is up.

## Invite Users

Users add the mint with an invite code and any node can create one, once the mint has reached consensus on a block height:

```bash
picomint-node-cli invite
```

The client can use this invite to download and verify the mint config from the node that generated it.

## Configure Gateways

The mint maintains an explicit list of recommended Lightning gateways. Any node can add a gateway; clients accept a gateway once a threshold of nodes recommends it.

Add a gateway:

```bash
picomint-node-cli gateway add <pk> <name>
```

Remove one:

```bash
picomint-node-cli gateway remove <pk>
```

List the current recommendations:

```bash
picomint-node-cli gateway list
```

## Backup

Once the setup ceremony completes, save your node's config to a file and
stash it somewhere safe (encrypted backup, password manager, paper
printout). The redirect happens on your machine, so the file lands there
even though the command runs in the container:

```bash
picomint-node-cli backup > backup.json
```

This single file is the only state you need to keep. It contains your
node's secret keys plus the mint's consensus config. The live
`database.redb` is operational state (BFT sessions, block sync) which is
reconstructed from nodes when a restored node rejoins.

If your deployment is ever lost, feed the backup to a fresh container's `setup restore` on stdin. This is the one command that needs `docker exec` to pass stdin through, so it is spelled out in full:

```bash
docker exec -i picomint-node-daemon picomint-node-cli setup restore < backup.json
```

## Announce Expiry

A mint winds down by announcing an expiry date, optionally with the invite code of a successor mint for users to migrate to. Every node has to enter the exact same values; clients trust the announcement once a threshold of nodes agree on it byte for byte:

```bash
picomint-node-cli expiry set --timestamp <unix-seconds> [--successor <invite>]
```

`expiry status` shows this node's announcement and `expiry clear` withdraws it.

## Sweep

Only once the mint has expired and its last onchain transaction has confirmed are the remaining funds swept with `picomint-rugpull`, a standalone x86_64 Linux binary. Like the images it is rebuilt on every push to main, attached to the [`main-latest`](https://github.com/joschisan/picomint/releases/tag/main-latest) pre-release; it runs on Ubuntu 24.04 or newer. It is not part of any image: it runs on an operator's machine, against that operator's bitcoind, with nothing but the secrets below.

Every node exports its sweep secret, which is bound to the mint's current UTXO. Check first that `onchain status` reports the same `tx_tip` on every node and `onchain pending` is empty everywhere, then:

```bash
picomint-node-cli onchain sweep
```

A threshold of nodes send their secrets to whoever sweeps. That operator passes the mint size, the destination address and the secrets; the tool interpolates the key, finds the UTXO in bitcoind's UTXO set, drains it to the address and broadcasts:

```bash
picomint-rugpull <nodes> <address> --bitcoind-url http://user:pass@127.0.0.1:8332 --secret <secret> --secret <secret> ...
```

The fee rate defaults to bitcoind's estimate; `--fee-rate-sat-per-vb` overrides it. The network is whatever bitcoind runs, and the address must be for it. If the tool reports no funds at the address the secrets reconstruct, a node exported before the last transaction confirmed, or a secret was copied wrong.

## Interfaces

| Port | Purpose                      | Safe to expose? |
|------|------------------------------|-----------------|
| 8080 | Iroh endpoint                | Yes             |

The admin CLI is a Unix socket at `{DATA_DIR}/cli.sock` — no port, no
network exposure, and the daemon keeps it and the data directory owner-only. Reach it with `docker exec picomint-node-daemon
picomint-node-cli …`.

## Configuration

| Env                          | Required | Default           | Description                                |
|------------------------------|----------|-------------------|--------------------------------------------|
| `DATA_DIR`                   | yes      |                   | Directory for the database file            |
| `BITCOIND_URL`               | yes      |                   | Bitcoin Core RPC URL with embedded credentials, e.g. `http://user:pass@127.0.0.1:8332`. A pruned node works within its prune window — see [Bitcoin Backend](#bitcoin-backend) above. The mint's network is read off the backend at DKG time. |
| `P2P_ADDR`                   | no       | `0.0.0.0:8080`    | Iroh endpoint listen address               |
