FROM rust:slim AS builder

RUN apt-get update && apt-get install -y musl-tools && rustup target add x86_64-unknown-linux-musl

WORKDIR /app

COPY . .

# Symbols are kept: a wedged daemon is diagnosed with `gdb -p` on the host,
# which needs them to name the functions each thread is blocked in.
RUN cargo build --release --target x86_64-unknown-linux-musl --bin picomint-client-daemon --bin picomint-client-cli \
    && mkdir /data

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-client-daemon /usr/local/bin/picomint-client-daemon
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-client-cli /usr/local/bin/picomint-client-cli

ENTRYPOINT ["picomint-client-daemon"]
