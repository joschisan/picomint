FROM rust:slim-trixie AS builder

WORKDIR /app

COPY . .

# Symbols are kept: a wedged daemon is diagnosed with `gdb -p` on the host,
# which needs them to name the functions each thread is blocked in.
RUN cargo build --release --bin picomint-lnurl-daemon

FROM gcr.io/distroless/cc-debian13:nonroot

COPY --from=builder /app/target/release/picomint-lnurl-daemon /usr/local/bin/picomint-lnurl-daemon

ENTRYPOINT ["picomint-lnurl-daemon"]
