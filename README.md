# Picomint - Alpha

Picomint is unfinished and unaudited, and currently refuses to run a mint on mainnet. Nothing is stable yet: a test deployment can break with any commit.

A minimal implementation of a federated Chaumian ecash mint on Bitcoin.

Three daemons, each with its own manual for the person who runs it:

- [**Node**](docker-node/README.md) — one of at least four nodes that form the mint, usually run at home.
- [**Gateway**](docker-gateway/README.md) — bridges mints to the Lightning Network, usually run in the cloud.
- [**Client**](docker-client/README.md) — a headless client for agents and testing. The app is the client for people.

What ships:

| Artifact | What it is | Manual |
|----------|------------|--------|
| `ghcr.io/joschisan/picomint-node-daemon:main` | The node, with `picomint-node-cli` on its `PATH` | [Node](docker-node/README.md) |
| `ghcr.io/joschisan/picomint-gateway-daemon:main` | The gateway, with `picomint-gateway-cli` on its `PATH` | [Gateway](docker-gateway/README.md) |
| `ghcr.io/joschisan/picomint-client-daemon:main` | The client daemon, with `picomint-client-cli` on its `PATH` | [Client](docker-client/README.md) |
| `picomint-sweep` | A Linux binary on the release page that drains a decommissioned mint's wallet | [Sweep](docker-node/README.md#sweep) |

## License

MIT.
