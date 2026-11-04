# One image holds every socialnet binary; each compose service / k8s Deployment
# picks one with its `command`. MODE selects the rpcstack policy stack at build
# time: `baseline` (no-op hooks) or `masa` (composed Masa policy).
#
#   docker build --build-arg MODE=masa -t rpcstack-socialnet:masa .

FROM rust:1-bookworm AS builder
ARG MODE=baseline
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev build-essential git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# rust-toolchain.toml pins the nightly; rustup installs it on first use.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target,id=socialnet-target-${MODE} \
    set -eu; \
    case "$MODE" in baseline|masa) ;; *) echo "MODE must be baseline or masa" >&2; exit 1;; esac; \
    jobs=$(nproc); [ "$jobs" -le 32 ] || jobs=32; \
    ulimit -n 65536 2>/dev/null || true; \
    cargo build --release --locked -j "$jobs" -p socialnet --features "socialnet/${MODE}"; \
    mkdir -p /out; \
    find target/release -maxdepth 1 -type f -perm -u+x ! -name '*.so' ! -name '*.d' -exec cp {} /out/ \;

FROM debian:bookworm-slim
ARG MODE=baseline
RUN apt-get update && apt-get install -y --no-install-recommends \
      libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/ /usr/local/bin/
ENV RPCSTACK_SOCIALNET_MODE=${MODE} \
    RUST_LOG=info
WORKDIR /app
