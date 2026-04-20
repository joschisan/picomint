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

Before the federation can start processing transactions, guardians run a one-time setup ceremony. The Web UI walks you through it in a setup wizard; the CLI does the same thing:

```bash
picomint-server-cli setup set-local-params <name> [--federation-name X] [--federation-size N]
picomint-server-cli setup add-peer <setup-code>
picomint-server-cli setup start-dkg
picomint-server-cli setup status
```

Exactly one guardian sets the global federation config and passes `--federation-name` and `--federation-size`; the others pass only their own `<name>`. Each guardian's `set-local-params` returns a setup code. Every guardian then calls `add-peer` once per peer with that peer's setup code. Once every guardian has added every peer, everyone has to run `start-dkg`. Run `setup status` to see your progress.

### Invite Users

Users join the federation with an invite code and any guardian can create one:

```bash
picomint-server-cli invite
```

The client can use this invite to download and verify the federation config from the guardian that generated it.

### Configure Gateways

The federation maintains an explicit list of recommended Lightning gateways. Any guardian can add a gateway and clients will priorititze gateways by the number of guardians recommending them.

```bash
picomint-server-cli module ln gateway add <url>
picomint-server-cli module ln gateway remove <url>
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
picomint-gateway-cli federation list
```

For the gateway to actually route payments on behalf of a federation, its guardians also need to add the gateway's URL to their recommended list — see [Configure Gateways](#configure-gateways) above.

### Recovery

If your gateway deployment is ever corrupted you can recover your onchain funds and ecash from your twelve word mnemonic:

```bash
picomint-gateway-cli mnemonic
```

The mnemonic can be used with any Bip 39 compatible wallet to recover the onchain funds and with any Picomint wallet to recover the funds in the federations.  **The balance in your open lightning channels is lost.**

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
