FROM rust:slim AS builder

WORKDIR /app

COPY . .

RUN cargo build --release --bin picomint-guardian-daemon --bin picomint-guardian-cli \
    && strip target/release/picomint-guardian-daemon target/release/picomint-guardian-cli \
    && mkdir /data

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/release/picomint-guardian-daemon /usr/local/bin/picomint-guardian-daemon
COPY --from=builder /app/target/release/picomint-guardian-cli /usr/local/bin/picomint-guardian-cli

ENTRYPOINT ["picomint-guardian-daemon"]
