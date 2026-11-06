# socialnet on rpcstack

A Rust port of the [DeathStarBench](https://github.com/delimitrou/DeathStarBench)
social network: 14 gRPC microservices (frontend, compose-post, user, social
graph, timelines, post storage, text/URL/mention/media/unique-id helpers, and a
RabbitMQ-driven home-timeline writer) backed by MongoDB, Redis, Memcached and
RabbitMQ, plus a closed-loop-free, open-arrival load generator.

It is built on [rpcstack](https://github.com/rpcstack/rpcstack), which owns RPC
lifecycle hooks and scheduling between future polls. Two modes are built from
the same source: `baseline` (the framework defaults: no-op hooks, FIFO run
queue) and `policy` (the RPC policy of the editable [`policy/`](policy/README.md)
directory, which starts as a no-op, like `baseline`). The frontend serves the
benchmark's public API over HTTP as well as gRPC, matching DeathStarBench's
nginx/lua gateway.

| Service | Port | Backends | Called by |
| --- | --- | --- | --- |
| frontend | 8080 (gRPC), 5000 (HTTP) | | load generator, HTTP clients |
| compose-post | 8080 | | frontend |
| register-user | 8080 | | frontend |
| user | 8080 | user MongoDB, Redis | frontend, compose-post, register-user, social-graph |
| social-graph | 8080 | MongoDB, Redis | frontend, user, home-timeline |
| user-timeline | 8080 | MongoDB, Redis | frontend, compose-post |
| home-timeline | 8080 | Redis | frontend, compose-post |
| post-storage | 8080 | MongoDB, Memcached | compose-post, timelines |
| unique-id, media, text, user-mention, url-shorten | 8080 | (user-mention: MongoDB, Memcached; url-shorten: MongoDB, Memcached) | compose-post, text |
| write-home-timeline | none (RabbitMQ consumer) | RabbitMQ | |

## HTTP API

The frontend serves the routes, parameters, status codes and JSON shapes of the
nginx/lua gateway of DeathStarBench socialNetwork
(`nginx-web-server/lua-scripts/wrk2-api/...` and `.../api/user/...`) on port
5000, so one HTTP load generator (wrk2 scripts included) can drive both
applications. Every request enters the RPC stack like a `socialnet_client_bench`
request: the frontend creates its root context (API name, deadline = now + SLO)
and calls the frontend service in process, so client hooks, the frontend's
server hooks and its child RPCs run as for a gRPC client. The per-route SLO is
application config, `frontend.sloMs` in `config/socialnet.json` (not part of
`policy/`; every route defaults to 200 ms), stamped on the request as its
`grpc-timeout`. Write routes read a urlencoded form body, read routes the query
string, as in the lua scripts.

| Route | Parameters | 200 response | Errors |
| --- | --- | --- | --- |
| `POST /wrk2-api/post/compose` | `user_id`, `username`, `post_type` (0 post, 1 repost, 2 reply, 3 dm), `text`; optional `media_ids`, `media_types` (JSON arrays, used when both are present) | `Successfully upload post` | 400 `Incomplete arguments`; 500 `compost_post failure: ...` |
| `POST /wrk2-api/user/register` | `first_name`, `last_name`, `username`, `password`, `user_id` | `Success!` | 400; 500 `User registration failure: ...` (for example a duplicate username) |
| `POST /wrk2-api/user/follow`, `/unfollow` | `user_id` + `followee_id`, or `user_name` + `followee_name` | `Success!` | 400; 500 `Follow Failed: ...` / `Unfollow Failed: ...` |
| `GET /wrk2-api/user-timeline/read` | `user_id`, `start`, `stop` | JSON array of posts, newest first (the entries `start` .. `stop - 1`) | 400; 500 `Get user-timeline failure: ...` |
| `GET /wrk2-api/home-timeline/read` | `user_id`, `start`, `stop` | the same, for the user's home timeline | 400; 500 |
| `POST /api/user/login` | `username`, `password` | 302 to `../../main.html?username=...` with a `login_token` cookie (a JWT) | 400; 500 `User login failure: ...` |
| `GET /api/user/get_follower`, `/get_followee` | cookie `login_token` | `[{"follower_id": "..."}]` / `[{"followee_id": "..."}]` | 401 (no, bad or expired token) |
| `/api/{post/compose,user/register,user/follow,user/unfollow,user-timeline/read,home-timeline/read,user/user_info}`, `/upload-media`, `/get-media` | | | 501 |

A post is `{"post_id","creator":{"user_id","username"},"req_id","text","user_mentions":[{"user_id","username"}],"media":[{"media_id","media_type"}],"urls":[{"shortened_url","expanded_url"}],"timestamp","post_type"}`
with the 64-bit ids and the timestamp (milliseconds) as strings and `post_type`
the enum number, as the lua `_LoadTimeline` builds it. A request the policy
rejects or aborts (`/EarlyReturn`, admission) answers 504 (deadline exceeded)
or 503 with the policy's message; any other failed RPC answers 500, as the lua
gateway does.

Where it differs from the nginx/lua gateway:

* **Browser-facing `/api/...` variants** (cookie authentication, redirects to
  static pages this port does not ship) are not served except `login`,
  `get_follower` and `get_followee`; they answer 501 rather than being faked,
  as do `/api/user/user_info` (no RPC for it) and the media frontend's
  `/upload-media` and `/get-media`. `/wrk2-api/...` is what wrk2 uses.
* **Empty lists encode as `[]`** (lua's cjson encodes an empty table as `{}`).
* **A malformed number or JSON list is a 400** (the lua scripts raise, 500).
* `/api/user/login` answers 302 to `main.html`, which this port does not
  serve: a client that follows redirects gets a 404 there.
* The cookie-authenticated routes need `JWT_SECRET` on the frontend (the user
  service's secret; Compose and the manifests set it) and answer 501 without it.

Where the Rust services behind it differ from DeathStarBench (what a
correctness oracle comparing answers must know):

* **Registration** (`/wrk2-api/user/register`) stores the user and inserts it
  into the social graph, like DSB; it does not seed any data. There is no
  `datasets/` loader: a stack starts empty and users must be registered over
  HTTP (or gRPC) before they follow, log in or read timelines.
* **Composing a post does not check that the author exists**: the creator is
  built from the request's `user_id` and `username` (DSB does the same), and the
  load generator (`socialnet_client_bench`) composes posts for `user_1`..`user_1000`
  that were never registered.
* **User mentions are resolved against a seeded table, not the users**: the
  user-mention service wipes and fills its own `usermention-db` with 1,000,000
  fake users (`user1`..`user1000000`, ids 1..1000000) at start, and returns the
  matches with `@memcached` or `@mongodb` appended to the username (so
  `@user5` yields `user5@mongodb`, then `user5@memcached` once cached). Registered
  users are not in that table; their mentions are dropped. Mentioned users get the
  post on their home timeline.
* **Home timelines are written synchronously**: compose-post calls
  home-timeline directly (DSB goes through RabbitMQ and a consumer); the
  `write-home-timeline` consumer is deployed but nothing publishes to it. A
  follower sees a post as soon as the compose request returns. The author's own
  home timeline is not written (as in DSB).
* **Randomness**: post and user ids from unique-id and user-service are time and
  counter based (`user_id` is chosen by the caller on the HTTP route); URL
  shortening generates random 10-character suffixes, so shortened URLs differ
  between runs; `login` tokens embed the time. Nothing else in the services is random;
  the load generator's request stream is seeded (`Repeats`, per-request
  independent streams).
* **Passwords** are salted SHA-256, like DSB; a failed login is a gRPC
  `UNAUTHENTICATED` (500 here), an unknown user `NOT_FOUND` (500).
* **Follow** by id does not check that the users exist; by username it does
  (500 when either is unknown).
* Timeline reads use Redis `ZREVRANGE start .. stop - 1` and return `[]` when
  `stop <= start` or `start < 0` (as DSB).

The gRPC frontend (`ComposePost`, `RegisterUser`) is unchanged for
`socialnet_client_bench`; the proto gained `RegisterUserWithId`, `Login`,
`Follow`, `Unfollow`, `FollowWithUsername`, `UnfollowWithUsername`,
`GetFollowers`, `GetFollowees`, `ReadUserTimeline` and `ReadHomeTimeline` for the
HTTP routes. `docker run --rm -e SOCIALNET_HTTP=http://<frontend>:5000 rpcstack-socialnet:<mode> http-check`
(`socialnet_http_check`) validates every route's status and shape against a
live stack.

## Deploy in one command

```sh
MODE=policy docker compose up --build                     # Compose (omit MODE for baseline)
scripts/kind-up.sh && scripts/k8s-deploy.sh policy        # Kubernetes (or: baseline)
```

Layout: `k8s/base` (baseline) plus `k8s/overlays/policy`, rendered with kustomize.

## Quick start: Docker Compose

Requires Docker with Compose v2. Everything is built locally; nothing is pulled
except MongoDB, Redis, Memcached and RabbitMQ.

```sh
docker compose up --build                       # baseline + load generator
MODE=policy docker compose up --build           # the policy/ directory's policy
docker compose run --rm loadgen                 # run the load generator again
docker compose down -v
```

The frontend is published on `localhost:${FRONTEND_PORT:-8080}` (gRPC) and
`localhost:${FRONTEND_HTTP_PORT:-5000}` (HTTP); try
`curl -d 'first_name=a&last_name=b&username=alice&password=pw&user_id=1' localhost:5000/wrk2-api/user/register`.

The first build compiles the whole workspace and takes several minutes. Load
generator traces are written to `out/<mode>/`; `GEN_CONFIG=gen_config.overload.json`
selects the overload configuration.

## Quick start: Kubernetes (kind)

Requires Docker, [kind](https://kind.sigs.k8s.io/) and kubectl.

```sh
scripts/kind-up.sh                 # kind cluster "rpcstack-socialnet" + registry:2
scripts/k8s-deploy.sh policy       # build, push to the registry, kubectl apply (or: baseline)
LOADGEN=1 scripts/k8s-deploy.sh policy  # same, then run the load generator Job
kubectl -n rpcstack-socialnet get pods
scripts/k8s-teardown.sh --cluster  # delete namespace, cluster and registry
```

`k8s/base` holds plain manifests (`infra.yaml`: MongoDB, Redis, Memcached,
RabbitMQ; `socialnet.yaml`: a Deployment and Service per application service; a
`socialnet-config` ConfigMap generated from `config/`). `k8s/overlays/policy` only
switches the image tag (`rpcstack-socialnet:policy`). Every container uses
`imagePullPolicy: Always`, so a re-pushed tag is never shadowed by a stale cached image. `k8s-deploy.sh` renders with
`kubectl kustomize --load-restrictor LoadRestrictionsNone`, because the ConfigMap
reads `config/` outside the kustomization. The registry host port is the first
free one from 5601 (shared hosts), so the scripts rewrite the manifests'
`localhost:5001` to it; set `REG_PORT`, `KIND_CLUSTER`, `REGISTRY`,
`SOCIALNET_NAMESPACE` (the whole stack is created in it), `SOCIALNET_IMAGE_NAME`
or `BUILDKIT_ADDR` (build and push with `buildctl` against that BuildKit daemon)
to override, for example to use a shared cluster.
`k8s/loadgen-job.yaml` is the load generator Job, applied by the scripts.

## Modes

The mode is fixed at build time, by a Cargo feature, so each mode is a separate
image (`rpcstack-socialnet:baseline`, `rpcstack-socialnet:policy`). The mode flows
Cargo feature, Docker build arg `MODE`, Compose `MODE` env or k8s overlay.
`docker run --rm rpcstack-socialnet:<mode> mode` prints what an image was built with.

| Mode | Cargo feature | Generated stubs | Run queue |
|---|---|---|---|
| `baseline` | `socialnet/baseline` | `tonic::rpcstack::noop::NoopHooks` | rpcstack's default FIFO |
| `policy` | `socialnet/policy` | `socialnet_policy::Hooks` (the `policy/` directory's module stack; initially empty) | `policy/queue/queue.rs` through `sched_custom` (initially plain FIFO) |

The `policy/` crate is the only directory an optimization agent edits; see
[`policy/README.md`](policy/README.md). Its initial content is a no-op, so
`policy` behaves like `baseline`. The application depends on the rpcstack
framework core only (no published policy such as Masa). Generated stubs default
to no-op hooks in rpcstack, so `build.rs` sets `.default_hooks_path(...)` for
every proto and fails the build if a generated stub does not default to the
mode's hooks (or, in `policy` mode, names `NoopHooks`); `crates/socialnet/tests/hooks.rs`
and `policy/tests/wiring.rs` check the stubs and the run queue. Scheduling
operates between future polls; there is no forced preemption.

Modes are built one at a time with explicit packages (`cargo build -p socialnet
--features policy`), not `--workspace`: the policy crate enables rpcstack
features that Cargo would otherwise unify into a baseline build.

## The rpcstack submodule

`rpcstack/` is a shallow git submodule of the framework at the same commit as
the Cargo git dependencies (`de0cedc`), so a person or agent working here can
read the framework, its `examples/` (published policies) and `docs/`. Fetch it
with `git submodule update --init rpcstack`. It is read-only reference: the
build uses Cargo git dependencies only (the workspace `exclude`s it, `.dockerignore`
skips it, CI and the image build never initialize it), and `scripts/check.sh`
fails if the submodule commit and every rpcstack rev in `Cargo.toml` and
`Cargo.lock` are not the same.

## Configuration and image entrypoint

The image's entrypoint (`docker/entrypoint.sh`) maps its first argument (a
service name such as `frontend-service`, or `client-bench`, `probe`, `http-check`,
`mode`) to a binary. `config/` (the load generator configs and `socialnet.json`,
the frontend's per-route SLOs) is baked into `/etc/socialnet` and overridden by
Compose's mount of `./config`, or by the `socialnet-config` ConfigMap on
Kubernetes.

## Deliberate differences from the application template

These follow from how socialnet is built, and are kept on purpose.

* **Services are configured through environment variables**, as in the DeathStarBench
  original, so there is no `config/<app>.json` of service addresses; `config/`
  holds the load generator configs and `socialnet.json` (the HTTP frontend's
  SLOs), and the Compose/k8s manifests carry each service's environment.
* **Services dial `<name>-1:8080`.** The application's replica-aware channel
  dials `<name>-<n>`, so Compose network aliases and k8s Services are named
  `<service>-1` (the stack runs one replica of each service).
* **`write-home-timeline-service` has no listener**: it consumes RabbitMQ, so it
  has no port, Service or readiness probe.
* **Cargo build jobs are capped at 32** in the image build (`-j min(nproc, 32)`),
  so building on a large shared host does not exhaust file descriptors/memory.
* **The registry port is chosen at deploy time** rather than fixed at 5001, so
  several clusters can share a host.
* **The smoke tests delete their kind cluster** by default (`KEEP=1` keeps it;
  with `SKIP_KIND_UP=1` they use an existing cluster and delete only their
  namespace).
* **Probe**: the stack probe (`socialnet_policy_probe`) is policy-agnostic: it
  waits for an end-to-end ComposePost, sends control requests, and sends each
  of 13 services one ordinary request that must succeed. Both modes must pass.

## Layout

| Path | Contents |
|---|---|
| `crates/socialnet` | services (`src/<service>/`), protos, HTTP API (`src/frontend/http.rs`), load generator (`src/client`), stack probe (`src/probe`), HTTP API check (`src/http_check`) |
| `policy/` | the RPC policy of the `policy` mode: run queue and module stack (the only directory an optimization agent edits) |
| `rpcstack/` | read-only framework source (git submodule) |
| `crates/app-utils` | load-generation, statistics and runtime helpers |
| `config/` | load generator configs (`gen_config.json`, `gen_config.overload.json`) and `socialnet.json` (HTTP frontend SLOs) |
| `Dockerfile`, `docker/entrypoint.sh`, `docker-compose.yaml` | one image with all binaries; compose stack |
| `k8s/base`, `k8s/overlays/policy`, `k8s/loadgen-job.yaml` | kustomize manifests; load generator Job |
| `scripts/` | `test.sh`, `check.sh`, `format.sh`, smoke tests, `kind-up.sh`, `k8s-deploy.sh`, `k8s-teardown.sh`, `kind-down.sh` |

## Develop and test

```sh
./scripts/test.sh     # unit and regression tests, baseline and policy
./scripts/check.sh    # rustfmt, rpcstack pin (Cargo.toml, Cargo.lock, submodule), single tokio/hyper, -D warnings
./scripts/format.sh
```

## Smoke tests

```sh
scripts/smoke-compose.sh policy   # full stack under Compose (also: baseline)
scripts/smoke-k8s.sh policy       # full stack on kind (also: baseline)
```

Each brings the stack up (`SKIP_BUILD=1` / `PREBUILT=1` reuse a built image), then:

1. **Stack probe** (`socialnet_policy_probe`): waits for an end-to-end
   ComposePost, sends control requests (20/20 must succeed), then sends each of
   13 services one ordinary request with a generous SLO (13/13 must succeed).
2. **HTTP API check** (`socialnet_http_check`): registers two users, logs both
   in, follows, composes posts (with and without media), reads both timelines,
   lists followers and followees, unfollows, and checks the error statuses and
   the 501 routes (`HTTP CHECK PASS`).
3. **Load run** (`config/gen_config.json`): zero RPC errors and at least 95% success.

The initial no-op `policy` mode is judged exactly like `baseline`; a policy that
sheds load needs its own thresholds in the smoke scripts (`check_traces` has
`--expect-early-returns` for that). Results land in `out/`. CI runs the Compose
smoke per mode (`ci.yml`) and the kind smoke per mode (`k8s-smoke.yml`, manual
and nightly); each job restores a once-built image, so every check stays under
ten minutes.

## Provenance

Ported from `vic-lsh/masa` `apps/socialnet` at commit `c8486eb`, built on
rpcstack at `de0cedc080882469b316c8125b7ad17a7c93e7fb`. Experiment runner,
evaluation configs and policy sweeps from the original are intentionally not
included.
