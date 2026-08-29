FROM rust:slim AS builder

WORKDIR /app

COPY . .

RUN cargo build --release --bin picomint-lnurl-daemon \
    && strip target/release/picomint-lnurl-daemon

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/picomint-lnurl-daemon /usr/local/bin/picomint-lnurl-daemon

ENTRYPOINT ["picomint-lnurl-daemon"]
