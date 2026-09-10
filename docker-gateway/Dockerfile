FROM rust:slim-trixie AS builder

WORKDIR /app

COPY . .

# Symbols are kept: a wedged daemon is diagnosed with `gdb -p` on the host,
# which needs them to name the functions each thread is blocked in.
RUN cargo build --release --bin picomint-gateway-daemon --bin picomint-gateway-cli \
    && mkdir /data

FROM gcr.io/distroless/cc-debian13:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/release/picomint-gateway-daemon /usr/local/bin/picomint-gateway-daemon
COPY --from=builder /app/target/release/picomint-gateway-cli /usr/local/bin/picomint-gateway-cli

ENTRYPOINT ["picomint-gateway-daemon"]
