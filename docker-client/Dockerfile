FROM rust:slim-trixie AS builder

WORKDIR /app

COPY . .

# Symbols are kept: a wedged daemon is diagnosed with `gdb -p` on the host,
# which needs them to name the functions each thread is blocked in.
RUN cargo build --release --bin picomint-client-daemon --bin picomint-client-cli \
    && mkdir /data

FROM gcr.io/distroless/cc-debian13:nonroot

COPY --from=builder --chown=nonroot:nonroot /data /data
COPY --from=builder /app/target/release/picomint-client-daemon /usr/local/bin/picomint-client-daemon
COPY --from=builder /app/target/release/picomint-client-cli /usr/local/bin/picomint-client-cli

ENTRYPOINT ["picomint-client-daemon"]
