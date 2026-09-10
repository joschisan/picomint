FROM rust:slim-trixie AS builder

WORKDIR /app

COPY . .

# Symbols are kept: a wedged daemon is diagnosed with `gdb -p` on the host,
# which needs them to name the functions each thread is blocked in.
RUN cargo build --release --bin picomint-node-daemon --bin picomint-node-cli \
    && mkdir /data

FROM gcr.io/distroless/cc-debian13:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/release/picomint-node-daemon /usr/local/bin/picomint-node-daemon
COPY --from=builder /app/target/release/picomint-node-cli /usr/local/bin/picomint-node-cli

ENTRYPOINT ["picomint-node-daemon"]
