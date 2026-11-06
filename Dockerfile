# One image holds every socialnet binary; each compose service / k8s Deployment
# picks one by passing a service name to docker/entrypoint.sh. MODE selects the
# policy mode at build time (Cargo feature of the socialnet crate):
#   baseline  framework defaults: generated stubs use NoopHooks, FIFO run queue
#   policy    the RPC policy of the policy/ directory (run queue + module
#             stack); starts as a no-op, like baseline
#
#   docker build --build-arg MODE=policy -t rpcstack-socialnet:policy .

FROM rust:1-bookworm AS builder
ARG MODE=baseline
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev build-essential git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# rust-toolchain.toml pins the nightly; rustup installs it on first use.
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY policy ./policy
# The policy mode compiles the run queue into rpcstack-sched, which is a
# dependency, so the queue source must exist at build time (.cargo/config.toml
# points RPCSTACK_QUEUE_SOURCE at policy/queue/queue.rs; baseline ignores it).
COPY .cargo ./.cargo
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target,id=socialnet-target-${MODE} \
    set -eu; \
    case "$MODE" in baseline|policy) ;; *) echo "MODE must be baseline or policy" >&2; exit 1;; esac; \
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
RUN echo "$MODE" > /etc/socialnet-mode
COPY docker/entrypoint.sh /usr/local/bin/socialnet-entrypoint
COPY config/ /etc/socialnet/
ENV RUST_LOG=info \
    SOCIALNET_GEN_CONFIG=/etc/socialnet/gen_config.json
WORKDIR /app
ENTRYPOINT ["/usr/local/bin/socialnet-entrypoint"]
CMD ["mode"]
