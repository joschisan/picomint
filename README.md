# Picomint - Alpha

> [!CAUTION]
> **Experimental software. Do not run this.**
>
> Picomint is unfinished, unaudited and under active development. Do not run this on mainnet.

A minimal implementation of a federated Chaumian ecash mint on Bitcoin.

Three daemons, each with its own manual for the person who runs it:

- [**Node**](docker-node/README.md) — a member of a mint's federation, run at home by a guardian through the admin CLI, usually with an agent doing the typing.
- [**Gateway**](docker-gateway/README.md) — bridges mints to the Lightning Network, run by a Lightning routing-node operator.
- [**Client**](docker-client/README.md) — a headless client for machines: load generation, latency measurement and agents. The app is the client for people.

## License

MIT.
