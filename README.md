# socialnet on rpcstack

A Rust port of the [DeathStarBench](https://github.com/delimitrou/DeathStarBench)
social network: 14 gRPC microservices (frontend, compose-post, user, social
graph, timelines, post storage, text/URL/mention/media/unique-id helpers, and a
RabbitMQ-driven home-timeline writer) backed by MongoDB, Redis, Memcached and
RabbitMQ, plus a closed-loop-free, open-arrival load generator.

It is built on [rpcstack](https://github.com/rpcstack/rpcstack), which owns RPC
lifecycle hooks and scheduling between future polls. This repository contains
only the application; the policy lives in rpcstack's `examples/masa`.

## Quick start

Docker (everything is built locally, nothing is pulled except MongoDB, Redis,
Memcached and RabbitMQ):

```sh
docker compose up --build                       # baseline (no-op hooks) + load generator
MODE=masa docker compose up --build             # Masa composed policy
```

Kubernetes (kind plus a local registry; needs `docker`, `kind`, `kubectl`):

```sh
scripts/k8s-up.sh            # baseline
scripts/k8s-up.sh masa       # Masa policy
kubectl --context kind-rpcstack-socialnet -n socialnet logs -f job/loadgen
scripts/k8s-down.sh          # remove the cluster and registry
```

The first build compiles the whole workspace and takes several minutes.
Load generator traces are written to `out/<mode>/` (compose) and to
the `loadgen` pod's `/out` (Kubernetes).

## Modes

The mode is fixed at build time, by a Cargo feature, so each mode is a separate
image (`rpcstack-socialnet:baseline`, `rpcstack-socialnet:masa`).

| Mode | Cargo feature | Hooks |
|---|---|---|
| `baseline` | `socialnet/baseline` | `masa::DefaultHooks` is rpcstack's no-op hooks |
| `masa` | `socialnet/masa` | `sched_slo, abort_slo, signal_slack, est_mean_var, ac_pred` |

Generated stubs default to no-op hooks in rpcstack, so `build.rs` sets
`.default_hooks_path("masa::DefaultHooks")` for every proto, and
`crates/socialnet/tests/hooks.rs` fails if any stub does not. Scheduling
operates between future polls; there is no forced preemption.

## Layout

| Path | Contents |
|---|---|
| `crates/socialnet` | services (`src/<service>/`), protos, load generator (`src/client`), policy probe (`src/probe`) |
| `crates/app-utils` | load-generation, statistics and runtime helpers |
| `config/` | load generator configs (`gen_config.json`, `gen_config.overload.json`) |
| `Dockerfile`, `docker-compose.yaml` | one image with all binaries; compose stack |
| `k8s/`, `scripts/k8s-*.sh` | plain manifests; kind and registry scripts |
| `scripts/` | `test.sh`, `check.sh`, `format.sh`, smoke tests |

Services reach each other as `<service>-1:8080`: the application's
replica-aware channel dials `<name>-<n>`, so compose network aliases and
Kubernetes Service names follow that pattern. The stack runs one replica of
each service.

## Develop and test

```sh
./scripts/test.sh     # unit and regression tests, baseline and masa
./scripts/check.sh    # rustfmt, rpcstack pin, single tokio/hyper, -D warnings
./scripts/format.sh
```

Smoke tests (build the stack, check that RPCs really go through the hooks, run
load) are in `scripts/smoke-compose.sh <mode>` and `scripts/smoke-k8s.sh <mode>`.
They assert zero RPC errors, a success rate of at least 95%, and a policy-live
check (`socialnet_policy_probe`): an RPC carrying an already-expired deadline
must be rejected by Masa's hooks in `masa` mode (`/EarlyReturn?src=...` from
every service) and must not be in `baseline` mode. In `masa` mode they also run
a short overload and verify recovery.

## Provenance

Ported from `vic-lsh/masa` `apps/socialnet` at commit `c8486eb`, built on
rpcstack at `de0cedc080882469b316c8125b7ad17a7c93e7fb`. Experiment runner,
evaluation configs and policy sweeps from the original are intentionally not
included.
