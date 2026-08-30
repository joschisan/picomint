FROM rust:slim AS builder

RUN apt-get update && apt-get install -y musl-tools && rustup target add x86_64-unknown-linux-musl

WORKDIR /app

COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl --bin picomint-guardian-daemon --bin picomint-guardian-cli \
    && strip target/x86_64-unknown-linux-musl/release/picomint-guardian-daemon target/x86_64-unknown-linux-musl/release/picomint-guardian-cli \
    && mkdir /data

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-guardian-daemon /usr/local/bin/picomint-guardian-daemon
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-guardian-cli /usr/local/bin/picomint-guardian-cli

ENTRYPOINT ["picomint-guardian-daemon"]
