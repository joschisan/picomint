# docker-integration-test

A long-running, fully-local two-federation deployment for client-app
development.

Brings up:

- `bitcoind` on regtest, with a sidecar that mines one block every ten
  seconds
- 4 guardians for **Test Federation I** (`pm-fed1-guardian-0..3`)
- 4 guardians for **Test Federation II** (`pm-fed2-guardian-0..3`)
- 1 gateway (`pm-gateway`) joined to both federations
- 1 recurring daemon (`pm-recurringd`)

All services share a docker network. Resetting state is a single command:
`docker compose down -v`.

## Bring it up

The compose file pulls the prebuilt images CI publishes from `main`
(`ghcr.io/joschisan/picomint-{server,gateway,recurring}:main`). To bump
to a freshly built `main`:

```bash
cd docker-integration-test
docker compose pull
docker compose up -d
./setup.sh
```

`setup.sh` drives the DKG ceremony for each federation in turn, joins
the gateway to both, registers the gateway URL with both, and pegs in 1
BTC of seed liquidity into each federation. It prints the two invite
codes, the gateway URL, the recurring daemon URL, and the per-guardian
UI URLs at the end.

The gateway URL registered with the federations must be reachable from
external clients, so `setup.sh` auto-detects the host's public IPv4 via
`api.ipify.org` and uses `http://<public_ip>:8090`. Override with
`GATEWAY_URL=http://...:8090 ./setup.sh` if auto-detection isn't right
(e.g. behind NAT, or for purely local dev where you want
`http://localhost:8090`).

## Endpoints

| Service             | Host port                                   | Notes                                |
|---------------------|---------------------------------------------|--------------------------------------|
| bitcoind RPC        | `http://localhost:18443`                    | user `bitcoin` / pass `bitcoin`      |
| Federation I UIs    | `http://localhost:3000`..`3003`             | password `picomint`                  |
| Federation II UIs   | `http://localhost:3010`..`3013`             | password `picomint`                  |
| gateway API         | `http://localhost:8090`                     | LDK BOLT P2P on `9735`               |
| recurring API       | `http://localhost:8091`                     |                                      |

Within the compose network the same services are reachable as
`bitcoind:18443`, `fed1-guardian-0..3:8080`, `fed2-guardian-0..3:8080`,
`gateway:8080`, `recurringd:8080`.

## Lightning

The gateway boots an LDK node but starts with no channels and no funds.
For LN flows, fund it from the regtest miner wallet and open a channel
to a counterparty (a second LDK node container is not bundled here yet).
Onchain peg-in/out, ecash, and federation flows work without any LN
setup. Cross-federation lightning payments between clients of Federation
I and Federation II flow through the gateway as a normal LN hop.

## Reset

```bash
docker compose down -v
```

Wipes all guardian/gateway/bitcoind state. Re-run `up -d` and
`./setup.sh` to start fresh — both federations get fresh DKG-derived
keys and brand new invite codes.
