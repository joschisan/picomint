FROM rust:slim AS builder

WORKDIR /app

COPY . .

RUN cargo build --release --bin picomint-gateway-daemon --bin picomint-gateway-cli \
    && strip target/release/picomint-gateway-daemon target/release/picomint-gateway-cli \
    && mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/release/picomint-gateway-daemon /usr/local/bin/picomint-gateway-daemon
COPY --from=builder /app/target/release/picomint-gateway-cli /usr/local/bin/picomint-gateway-cli

ENTRYPOINT ["picomint-gateway-daemon"]
