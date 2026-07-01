FROM rust:1.95-slim AS builder

WORKDIR /src
ENV RUSTUP_TOOLCHAIN=1.95.0

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .

RUN cargo build --release --bin semfs

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    fuse3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/semfs /usr/local/bin/semfs

ENTRYPOINT ["semfs"]
CMD ["--help"]
