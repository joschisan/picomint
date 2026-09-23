# LNURL Daemon

`picomint-lnurl-daemon` is a stateless LNURL proxy service that allows picomint clients to receive LNURL payments via Lightning.

This service requires no database or persistent state. All payment information is encoded in the LNURL itself, making it easy to deploy on platforms like Digital Ocean App Platform, Fly.io, Railway, etc.

The one thing it keeps in memory is a pool of connections to every mint it has issued an invoice for, so requests after the first for a mint reuse warm connections. LUD-21 verify finds the mint in that pool by id, which means a verify only succeeds on the process that issued the invoice: after a restart, or on another replica behind a load balancer, it answers as a network failure would and the wallet keeps polling. The payment itself is unaffected; run a single instance if verify matters.

The operator of the service is trusted to provide the correct invoice to the requester, but does not take custody of the funds when the invoice is paid.

## How it works

1. Client generates an LNURL locally containing encoded payment details (recipient public key, the mint's node set, and an info commitment — no mint id or gateway list, which is what keeps the LNURL valid across gateway churn)
2. When a payer scans the LNURL, `GET /pay/{payload}` returns the LNURL-pay response
3. Payer requests invoice via `GET /invoice/{payload}?amount=X`
4. Server decodes payload, authors the incoming contract for the recipient, has a gateway of the mint issue an invoice against its payment hash, and returns the BOLT11 invoice
5. Payer pays the invoice directly to the gateway
6. Payer's wallet may confirm settlement via LUD-21 `GET /verify/...`, which the daemon answers from the mint over the pool the invoice request warmed: a threshold of its nodes attest the incoming contract was funded and hand out the preimage
7. Recipient claims funds from the mint when they come online

Note that once the invoice is generated, the daemon cannot claim the funds for itself.

## Command line options

```text
Usage: picomint-lnurl-daemon [OPTIONS]

Options:
      --api-addr <API_ADDR>  Public HTTP API listen address [env: API_ADDR=] [default: 0.0.0.0:8080]
  -h, --help                 Print help
```

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Health check |
| GET | `/pay/{payload}` | LNURL-pay first step (returns `PayResponse`) |
| GET | `/invoice/{payload}?amount=X` | LNURL-pay second step (returns invoice) |
| GET | `/verify/{mint_id}/{payment_hash}` | LUD-21 payment verification, threshold-read from the mint, on the process that issued the invoice (`?wait` long-polls) |

### Environment Variables

- `API_ADDR` - Public HTTP API listen address (default: `0.0.0.0:8080`)
