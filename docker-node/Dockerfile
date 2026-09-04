FROM rust:slim AS builder

RUN apt-get update && apt-get install -y musl-tools && rustup target add x86_64-unknown-linux-musl

WORKDIR /app

COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl --bin picomint-node-daemon --bin picomint-node-cli \
    && strip target/x86_64-unknown-linux-musl/release/picomint-node-daemon target/x86_64-unknown-linux-musl/release/picomint-node-cli \
    && mkdir /data

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-node-daemon /usr/local/bin/picomint-node-daemon
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-node-cli /usr/local/bin/picomint-node-cli

ENTRYPOINT ["picomint-node-daemon"]
