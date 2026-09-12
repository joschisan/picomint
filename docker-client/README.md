# Picomint Client Daemon

A headless client for machines: the client library behind an admin socket, with the same analytics mirror the gateway keeps. It exists for load generation, latency measurement and agents, not for people — the app is the client for people. Every command is one client call and returns what the call returns; sends hand back an operation id, and the outcome is read from the analytics through `query`.

```bash
docker run -d --name picomint-client-daemon \
    -e NETWORK=regtest \
    -v client-data:/data \
    ghcr.io/joschisan/picomint-client-daemon:main
```

`DATA_DIR` defaults to `/data`; `API_ADDR` (default `0.0.0.0:8080`) is where its iroh endpoint binds. Reach the CLI through `docker exec picomint-client-daemon picomint-client-cli ...`. Every command takes the mint id first and, where funds move, the account next:

```
add <invite>
remove <mint>
list
config <mint>
balance <mint> <account>
ecash count <mint> <account>
ecash send <mint> <account> <amount>
ecash send-max <mint> <account>
ecash receive <mint> <account> <ecash>
onchain send-fee <mint>
onchain send <mint> <account> <address> <amount> [--fee <amount>]
onchain send-max <mint> <account> <address>
onchain receive <mint> <account>
lightning send <mint> <account> <invoice>
lightning send-max <mint> <account> <lnurl>
lightning receive <mint> <account> <amount>
lightning lnurl <mint> <account> <lnurl-daemon-url>
lightning refresh-gateways <mint>
mnemonic
query <sql>
```

A self-payment loop through a gateway is a shell loop over `lightning receive` and `lightning send`; the acceptance latencies of every transaction it produces land in `core_tx_create` and `core_tx_accept`, and the payment outcomes in `lightning_send_success` and `lightning_send_refund`.
