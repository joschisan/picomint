FROM rust:slim AS builder

RUN apt-get update && apt-get install -y musl-tools && rustup target add x86_64-unknown-linux-musl

WORKDIR /app

COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl --bin picomint-gateway-daemon --bin picomint-gateway-cli \
    && strip target/x86_64-unknown-linux-musl/release/picomint-gateway-daemon target/x86_64-unknown-linux-musl/release/picomint-gateway-cli \
    && mkdir /data

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-gateway-daemon /usr/local/bin/picomint-gateway-daemon
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-gateway-cli /usr/local/bin/picomint-gateway-cli

ENTRYPOINT ["picomint-gateway-daemon"]
