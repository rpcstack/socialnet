# socialnet on rpcstack

A Rust port of the [DeathStarBench](https://github.com/delimitrou/DeathStarBench)
social network: 14 gRPC microservices (frontend, compose-post, user, social
graph, timelines, post storage, text/URL/mention/media/unique-id helpers, and a
RabbitMQ-driven home-timeline writer) backed by MongoDB, Redis, Memcached and
RabbitMQ, plus a closed-loop-free, open-arrival load generator.

It is built on [rpcstack](https://github.com/rpcstack/rpcstack), which owns RPC
lifecycle hooks and scheduling between future polls. This repository contains
only the application; the policy lives in rpcstack's `examples/masa`.

## Deploy in one command

```sh
MODE=masa docker compose up --build                       # Compose (omit MODE for baseline)
scripts/kind-up.sh && scripts/k8s-deploy.sh masa          # Kubernetes (or: baseline)
```

Layout: `k8s/base` (baseline) plus `k8s/overlays/masa`, rendered with kustomize.

## Quick start: Docker Compose

Requires Docker with Compose v2. Everything is built locally; nothing is pulled
except MongoDB, Redis, Memcached and RabbitMQ.

```sh
docker compose up --build                       # baseline (no-op hooks) + load generator
MODE=masa docker compose up --build             # Masa composed policy
docker compose run --rm loadgen                 # run the load generator again
docker compose down -v
```

The first build compiles the whole workspace and takes several minutes. Load
generator traces are written to `out/<mode>/`; `GEN_CONFIG=gen_config.overload.json`
selects the overload configuration.

## Quick start: Kubernetes (kind)

Requires Docker, [kind](https://kind.sigs.k8s.io/) and kubectl.

```sh
scripts/kind-up.sh                 # kind cluster "rpcstack-socialnet" + registry:2
scripts/k8s-deploy.sh masa         # build, push to the registry, kubectl apply (or: baseline)
LOADGEN=1 scripts/k8s-deploy.sh masa    # same, then run the load generator Job
kubectl -n rpcstack-socialnet get pods
scripts/k8s-teardown.sh --cluster  # delete namespace, cluster and registry
```

`k8s/base` holds plain manifests (`infra.yaml`: MongoDB, Redis, Memcached,
RabbitMQ; `socialnet.yaml`: a Deployment and Service per application service; a
`socialnet-config` ConfigMap generated from `config/`). `k8s/overlays/masa` only
switches the image tag (`rpcstack-socialnet:masa`). `k8s-deploy.sh` renders with
`kubectl kustomize --load-restrictor LoadRestrictionsNone`, because the ConfigMap
reads `config/` outside the kustomization. The registry host port is the first
free one from 5601 (shared hosts), so the scripts rewrite the manifests'
`localhost:5001` to it; set `REG_PORT`, `CLUSTER` or `REGISTRY` to override.
`k8s/loadgen-job.yaml` is the load generator Job, applied by the scripts.

## Modes

The mode is fixed at build time, by a Cargo feature, so each mode is a separate
image (`rpcstack-socialnet:baseline`, `rpcstack-socialnet:masa`). The mode flows
Cargo feature, Docker build arg `MODE`, Compose `MODE` env or k8s overlay.
`docker run --rm rpcstack-socialnet:<mode> mode` prints what an image was built with.

| Mode | Cargo feature | Hooks |
|---|---|---|
| `baseline` | `socialnet/baseline` | `masa::DefaultHooks` is rpcstack's no-op hooks |
| `masa` | `socialnet/masa` | `sched_slo, abort_slo, signal_slack, est_mean_var, ac_pred` |

Generated stubs default to no-op hooks in rpcstack, so `build.rs` sets
`.default_hooks_path("masa::DefaultHooks")` for every proto, and
`crates/socialnet/tests/hooks.rs` fails if any stub does not. Scheduling
operates between future polls; there is no forced preemption.

## Configuration and image entrypoint

The image's entrypoint (`docker/entrypoint.sh`) maps its first argument (a
service name such as `frontend-service`, or `client-bench`, `probe`, `mode`) to a
binary. `config/` (the load generator configs) is baked into `/etc/socialnet` and
overridden by Compose's mount of `./config`, or by the `socialnet-config`
ConfigMap on Kubernetes.

## Deliberate differences from the application template

These follow from how socialnet is built, and are kept on purpose.

* **Services are configured through environment variables**, as in the DeathStarBench
  original, so there is no `config/<app>.json` of service addresses; `config/`
  holds only the load generator configs, and the Compose/k8s manifests carry
  each service's environment.
* **Services dial `<name>-1:8080`.** The application's replica-aware channel
  dials `<name>-<n>`, so Compose network aliases and k8s Services are named
  `<service>-1` (the stack runs one replica of each service).
* **`write-home-timeline-service` has no listener**: it consumes RabbitMQ, so it
  has no port, Service or readiness probe.
* **Cargo build jobs are capped at 32** in the image build (`-j min(nproc, 32)`),
  so building on a large shared host does not exhaust file descriptors/memory.
* **The registry port is chosen at deploy time** rather than fixed at 5001, so
  several clusters can share a host.
* **The smoke tests delete their kind cluster** by default (`KEEP=1` keeps it).
* **Probe**: the policy-live probe sends each service an RPC with an
  already-expired deadline (as in the template) and in `baseline` the smoke also
  runs it with `--expect masa` as a negative control that must fail.

## Layout

| Path | Contents |
|---|---|
| `crates/socialnet` | services (`src/<service>/`), protos, load generator (`src/client`), policy probe (`src/probe`) |
| `crates/app-utils` | load-generation, statistics and runtime helpers |
| `config/` | load generator configs (`gen_config.json`, `gen_config.overload.json`) |
| `Dockerfile`, `docker/entrypoint.sh`, `docker-compose.yaml` | one image with all binaries; compose stack |
| `k8s/base`, `k8s/overlays/masa`, `k8s/loadgen-job.yaml` | kustomize manifests; load generator Job |
| `scripts/` | `test.sh`, `check.sh`, `format.sh`, smoke tests, `kind-up.sh`, `k8s-deploy.sh`, `k8s-teardown.sh`, `kind-down.sh` |

## Develop and test

```sh
./scripts/test.sh     # unit and regression tests, baseline and masa
./scripts/check.sh    # rustfmt, rpcstack pin, single tokio/hyper, -D warnings
./scripts/format.sh
```

## Smoke tests

```sh
scripts/smoke-compose.sh masa     # full stack under Compose (also: baseline)
scripts/smoke-k8s.sh masa         # full stack on kind (also: baseline)
```

Each brings the stack up (`SKIP_BUILD=1` / `PREBUILT=1` reuse a built image), then:

1. **Policy-live probe** (`socialnet_policy_probe`): waits for an end-to-end
   ComposePost, sends control requests, then sends each of 13 services an RPC
   with an already-expired deadline. In `masa` mode every service must reject it
   with `/EarlyReturn?src=...` (13/13); in `baseline` none may. In `baseline` the
   smoke also runs the probe with `--expect masa` as a negative control that must fail.
2. **Load run** (`config/gen_config.json`): zero RPC errors and at least 95% success.
3. **Overload and recovery** (`masa` only; `config/gen_config.overload.json`): the
   policy must shed load with `/EarlyReturn`, with no transport errors, then a
   normal run must recover to at least 95% success.

Results land in `out/`. CI runs the Compose smoke per mode (`ci.yml`) and the
kind smoke per mode (`k8s-smoke.yml`, manual and nightly); each job restores a
once-built image, so every check stays under ten minutes.

## Provenance

Ported from `vic-lsh/masa` `apps/socialnet` at commit `c8486eb`, built on
rpcstack at `de0cedc080882469b316c8125b7ad17a7c93e7fb`. Experiment runner,
evaluation configs and policy sweeps from the original are intentionally not
included.
