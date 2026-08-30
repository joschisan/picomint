FROM rust:slim AS builder

RUN apt-get update && apt-get install -y musl-tools && rustup target add x86_64-unknown-linux-musl

WORKDIR /app

COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl --bin picomint-lnurl-daemon \
    && strip target/x86_64-unknown-linux-musl/release/picomint-lnurl-daemon

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/picomint-lnurl-daemon /usr/local/bin/picomint-lnurl-daemon

ENTRYPOINT ["picomint-lnurl-daemon"]
