FROM rust:1.85-bookworm AS builder

WORKDIR /src
COPY rust /src/rust
COPY benchmarks/blind/openai-cross-provider.jsonl /src/benchmarks/blind/openai-cross-provider.jsonl
RUN cargo build \
    --manifest-path rust/Cargo.toml \
    --package wayfinder-cli \
    --bin wayfinder-router \
    --features otel \
    --release \
    --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 wayfinder \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /nonexistent wayfinder \
    && install -d -o root -g root -m 0755 /etc/wayfinder \
    && install -d -o 10001 -g 10001 -m 0750 /var/lib/wayfinder
COPY --from=builder /src/rust/target/release/wayfinder-router /usr/local/bin/wayfinder-router

ENV WAYFINDER_CONFIG=/etc/wayfinder/wayfinder-router.toml \
    WAYFINDER_ROUTER_AUDIT_FILE=/var/lib/wayfinder/wayfinder-audit.jsonl \
    WAYFINDER_ROUTER_SAVINGS_FILE=/var/lib/wayfinder/wayfinder-savings.json

WORKDIR /var/lib/wayfinder
USER 10001:10001
EXPOSE 8088

CMD ["wayfinder-router", "serve", "--surface", "data-plane", "--host", "0.0.0.0", "--port", "8088", "--config", "/etc/wayfinder/wayfinder-router.toml"]
