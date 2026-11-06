# rpc-policy-k8s: the social network's RPC policy, on Kubernetes

The vibesys optimization task of this repository: an agent edits only `policy/` (the run
queue and the module stack of rpcstack) and is scored on the social network's throughput
(closed loop) and p99 latency (open loop at a fixed rate) on Kubernetes, behind an accuracy
check whose oracle judges every answer of a recorded history. The agent's brief is
[OBJECTIVE.md](OBJECTIVE.md); this file is for whoever maintains the task. It mirrors the
hotel's task of rpcstack/hotel (same layout, objectives and mutant approach).

It runs on vibesys-evaluator 0.3.0rc2: the Go SDK (`github.com/uw-syfi/vibesys/sdk/
vibesys-evaluator/go v0.3.0-rc.2`, in `evaluator/go.mod`) and the `vibesys-evaluator==0.3.0rc2` PyPI package
(`Dockerfile`, on a digest-pinned python base; the private ghcr evaluator image is not used because
the in-cluster rebuild cannot pull it).
Both pins move together.

## Layout

| Path | What it is |
| --- | --- |
| `vibesys.input.toml` | The task (schema 0.2): `benchmark` and `accuracy` commands, both `/task/rpc-policy-k8s` |
| `objectives.toml` | The two objectives and their noise |
| `OBJECTIVE.md` | The agent's brief: objectives, edit scope, validity, correctness, rules |
| `Dockerfile` | The task image: the evaluator image plus the task's Go programs in `/task` |
| `k8s/socialnet.yaml`, `k8s/render.sh` | The deployed manifest, rendered from the repository's `k8s/base` (no Namespace, `${SOCIALNET_IMAGE}`); `render.sh --check` says whether it is stale |
| `scope.json` | The edit scope: digests of every file outside `policy/` the image build reads, and `Cargo.lock`'s packages (`/task/scope write` regenerates it) |
| `evaluator/cmd/rpc-policy-k8s` | The task binary: `microservices.Main` (benchmark, accuracy check, the pods' commands) behind the scope guard |
| `evaluator/cmd/oracle` | The oracle offline: `oracle --history DIR` judges a captured history again, `oracle stats DIR` says what it exercised |
| `evaluator/cmd/scope` | Writes or checks `scope.json` |
| `evaluator/cmd/explore` | The benchmark at other loads (for choosing them; not a task command) |
| `evaluator/internal/social` | The application as the task sees it: the dataset and its seed, the HTTP routes, the served posts and what a compose stores |
| `evaluator/internal/workload` | The benchmark's mix (`bench.go`) and the accuracy workload (`accuracy.go`) |
| `evaluator/internal/oracle` | The oracle's specs (`oracle.go`) and the facts they share (`facts.go`) |
| `evaluator/internal/task` | The wiring: the App, the benchmark, the accuracy check, the faults, the scope guard |
| `evaluator/internal/scope` | The scope check |
| `tests/mutants/*.patch`, `tests/mutants.sh` | Policy mutants (each a change of `policy/` only) that the accuracy check must fail |

## The application on Kubernetes

`k8s.App`: the manifest `k8s/socialnet.yaml` (13 services, 8 MongoDB, Redis and Memcached
pods, RabbitMQ, as `scripts/k8s-deploy.sh` applies them; every service one replica, no
resource limits), one image built from the candidate's root `Dockerfile` (`Images:
{SOCIALNET_IMAGE: "."}`), the gateway `http://frontend-service:5000`. The root `Dockerfile`'s
default `MODE` is now `policy`, since the evaluator builds without build arguments; every
other user of it (Compose, CI, `scripts/k8s-deploy.sh`) passes `MODE` explicitly.

The application starts empty (only the user-mention table of a million fake users is filled,
by that service at start), so the task seeds it: `Task.Seed` (`social.Dataset.Populate`)
registers 1,000 users `user<id>` (password `pw<id>`) and the follow edges of the dataset over
the HTTP API, from a seed pod, before the phases. The graph (`internal/social/dataset.go`) is
generated from a seed: each user follows 8 to 40 others drawn by popularity (Zipf, exponent
0.9), so user 1 is the most followed. Users with ids up to a million can be mentioned (the
mention table), but only registered ones have timelines. The readiness steps are a read of
each timeline of user 1.

## The benchmark

One sample: 2 closed-loop phases of 32 clients (60 s after 15 s warmup, the median),
then 3 open-loop phases at 800 ops/s, uniform arrivals (60 s after 10 s warmup, the median
of the p99s). Validity: no error in a window, every operation at least once, the open loop at
95% of its rate (the DeathStarBench and TrainTicket rules).

The mix is the DeathStarBench social network task's (`social-network-k8s`, as its Go workload
draws it), over this dataset: `user_timeline_read` 500 (a user drawn by popularity, Zipf with
exponent 1), `home_timeline_read` 400 (a user drawn by activity, Zipf 0.75), `compose_post` 100
(an active user; 2 to 7 words and a unique marker, a mention of a followee 20%, a URL at the
end 15%, media 5%; then a read of its own user timeline) and `follow` / `unfollow` 5 each.
Pages are 10 posts: the first 80%, the second 12%, the third 5%, pages 4 to 7 3%. The
benchmark has no oracle; an answer is only counted.

### Choosing the loads

`cmd/explore` runs the benchmark at other loads (`EXPLORE_CLIENTS`, `EXPLORE_RATES`, with
`EXPLORE_CAPTURE=1` it keeps the bodies, so the errors can be read). On the base, on
vibesys-k8s, 30 s phases, one cluster state after another:

- Closed loop: 16 clients 5,800 ops/s, 32 clients 3,300 to 5,600, 48 clients 2,100, 64 clients
  2,100, 128 clients: the stack breaks (every request answers 500 "transport error" from then
  on). Throughput falls as clients are added: the application collapses under contention, which
  is what a scheduler or an admission policy can improve. Within one sample it also falls from
  one phase to the next (4,350 then 2,150 ops/s at 32 clients), as posts and timelines
  accumulate; the median of the two trials is a mean, and the samples agree (below).
- Open loop: 800 to 1,000 ops/s answer in 1 ms (p50) with a p99 of 30 to 60 ms; 2,000 ops/s and
  above fail requests ("transport error"), so the open rate is 800, clearly below saturation.
- 32 clients is the largest count that does not break the stack, with margin; a policy that
  makes the stack break fails the validity rule (an error in a window).

Two application defects limit the loads, and the first is why the repository's base app is
changed by this task's pull request (not a policy matter): user-timeline-service opened a new
Redis connection for every command and ran out of ephemeral ports ("Cannot assign requested
address") at a few hundred requests per second, so no load of the benchmark was error free;
it now keeps one multiplexed connection and reconnects when it breaks
(`crates/socialnet/src/user_timeline/server.rs`). The other is left as it is: after the first
failed requests the post-storage service's one Memcached connection wedges ("Buffer length is
less than last read length") and every read fails until it restarts, which is also why the
accuracy check does not restart that Memcached.

### Baseline

Three samples of the base (`rpc-policy-k8s benchmark`, vibesys-k8s, the final image):

| Sample | operations_per_second | latency_ms.p99 |
| --- | --- | --- |
| 1 | 3,257 | 53.95 |
| 2 | 3,333 | 54.82 |
| 3 | 3,253 | 54.34 |

Spread (max over min) 2.5% and 1.6%; `objectives.toml` declares noise floors of 5% and 8%,
which leaves room for the cluster's other tenants.

## The accuracy check

Four open-loop phases at 300 ops/s (uniform), 45 s each, the whole history captured; faults
between them: after phase 0 the frontend, compose-post, user-timeline, home-timeline,
social-graph and post-storage services restart one after another; after phase 1 the five
caches with a store behind them are flushed in place (`microservices.Flush`: the user-timeline,
social-graph and user Redis, the post-storage and user-mention Memcached); after phase 2 every
service restarts at once, and so do the user-timeline and social-graph Redis pods.
The post-storage Memcached pod is not restarted: the post-storage service keeps its one
connection and never reconnects, so every timeline read answers 500 afterwards (a flush is
fine). MongoDB is not restarted (its data lives in an `emptyDir`), and neither is
`home-timeline-redis`, which is not a cache: nothing stores home timelines but that Redis, so
a flush or restart would lose posts the application never gets back.

### What the application does that the oracle models (checked in the code)

- **Home timelines are written synchronously**, by compose-post, before it answers: the
  followers (read from the social graph) and the mentioned users get the post. A follower that
  was added before the compose has it; one that unfollowed before does not; an overlapping
  follow change may go either way. A post stays on a home timeline after an unfollow.
- **The author's own home timeline is not written.**
- **Compose does not check that the author exists**, and **mentions resolve against 1,000,000
  seeded fake users** (`@user<id>` is the user `<id>`; the answer's username is
  `user<id>@mongodb` or `user<id>@memcached`, whichever answered), not against the registered
  users. The workload mentions hot users only, so the mentioned user is a registered one.
- **A timeline is a Redis sorted set scored by the post's timestamp** (set by compose-post
  inside the request); two posts in one millisecond have no defined order, and the oracle
  treats timestamps as intervals (below). A user timeline falls back to MongoDB for what Redis
  does not hold, so a flush changes nothing.
- The user timeline races with itself after a flush, and the social graph keeps a partial
  follower set after a flush, as described below; the workload avoids both.

### The workload (`internal/workload/accuracy.go`)

The users are the 40 hot ones (ids 1 to 40, the most followed, 413 of the graph's edges are
among them), and operation i of a phase is user (i mod 40)'s: composes 24%, home timeline
reads 34%, user timeline reads 26%, follows 6%, unfollows 6%, invalid requests 4%. At 300
ops/s a user's operations are 133 ms apart, so its own requests never overlap in practice
(the oracle still explores overlap when one does). Reads are of the user's own timeline.
The application has races of its own that a strict oracle would blame on the candidate, so the
workload avoids them:

- The user-timeline service reads Redis and then MongoDB without a lock, so a read racing a
  post of the same author after a flush can skip an entry: an author's reads and posts are
  one user's operations, a round apart.
- The social-graph service adds a follow to the followee's cached follower set even when the
  cache does not hold it (after a flush), which leaves a partial set that later posts are
  delivered by. A follow change of u -> v is sent only after v posted at least one round
  earlier in the phase (a post reads v's followers and fills the cache), and otherwise becomes
  a follow without a followee (a 400, nothing changes). Changes are between hot users and never
  touch a seeded edge, so a follower set never empties (an empty set is refilled from MongoDB
  by the next post, racing an unfollow).
- Every post has a unique marker (`#m...`), mentions another hot user 20% of the time, ends
  with a URL 10% and has media 5%. Reads are a page of 10: the first 70%, the second 20%, the
  third 7%, pages 4 to 7 3%.
- A few requests are invalid on purpose: a post without text, a follow without a followee, a
  read of user `abc`, of an empty window, of a negative start.

### The oracle (`internal/oracle`)

Three specs of vibesys-evaluator's `check` package, all of which must hold (the design is in
the package comment of `oracle.go`):

- `author`: one key per hot author. The state is the set of its posts; a compose adds its
  post (an unknown outcome may or may not have), a read of the user timeline must show exactly
  its window of them, newest first.
- `reader`: one key per hot reader. The state is the reader's follow edges (from the seeded
  graph) and the set of posts delivered to it. A compose is a piece of every reader that can
  receive it (a seeded follower, a reader with a follow change for the author, the mentioned
  user) and delivers when the edge is on at its linearization point or the post mentions the
  reader; a follow change sets or clears the edge. A read of the home timeline must show
  exactly its window of the delivered posts. This is the replication the evaluator docs warn
  about, bounded here by 40 readers.
- `responses`: every call once, unknown when lost or 5xx (so the 1% limit on unknown outcomes
  counts calls); every answer on its own: status and body, each served post's text, creator,
  mentions, URLs and media against its compose (`social.Composed.Check`), one timestamp and
  post id whenever a post is served, a timestamp within the compose's call window (with a
  500 ms slack), pages newest first, own posts not on the home timeline, and the invalid
  requests' refusals.

Both timeline specs judge a window in intervals, because the order is by timestamp and the
application takes it inside the compose: a post's timestamp is the one it was first served
with, or, until then, bounded by its compose's call window. A page is right when the posts that
certainly sort before its first entry number at most its start and the posts that may sort
before it at least its start, no delivered post certainly sorts inside it, and a short page is
the end of the list (`checkPage`). Two passes: the first collects each compose, where it was
first served, the follow changes and the readers; the second adds the pieces.

### Results

The base passes (vibesys-k8s-2, the final application and image): 54,000 calls, none lost, 26,980
author pieces in 40 partitions and 526,516 reader pieces in 40 partitions, all checked (no reads
sampled out), 5 s of checking, peak heap 1.4 GB, most operations in flight at once on one key: 1
(author) and 5 (reader; the evaluator's advice is 12 or fewer). A run takes about 8 minutes,
image builds included. What it exercised (`oracle stats`): 12,897 composes (2,576 with a mention;
all but 3 served at least once), 14,083 user timeline and 18,343 home timeline reads (18,310 of
the home ones non-empty; 319,000 served posts, 9,400 reads past the first page), 6,502 follow
and unfollow changes answered, 2,150 invalid requests of the five kinds, 40 readers and 40
authors.

The mutants (below) on the same cluster: `shed-home-write` FAIL (every reader partition
illegal, found within the first 100 operations), `abort-after-compose` FAIL (16% unknown
outcomes, over the 1% limit, before any search), `shed-user-write` FAIL (author spec: 40 author partitions illegal, found in 9.7 s of checking).

`stale-timeline-cache` (a 2 s memo of the timeline reads keyed by user and window, in the
style of the hotel's availability cache) was tried and dropped: the policy compiles and
deploys, but the check passes it, because its cache almost never answers (7 of 9,824
first-page user reads were stale in a run), so it is not a mutant of the application's
behaviour; a unit test of the oracle covers a stale page instead (`TestStaleUserTimelineFails`,
`TestMissingNewestHomePostFails`).

## Run it

On a cluster (the environment vibesys gives a job: `KUBECONFIG`, `VIBESYS_K8S_REGISTRY`,
`VIBESYS_K8S_BUILDER`), from the repository root:

```sh
docker build -t rpc-policy-k8s-task .vibesys/tasks/rpc-policy-k8s
run() { docker run --rm --init --network host --user "$(id -u):$(id -g)" \
  -v "$PWD:/candidate:ro" -v "$PWD/out:/vibesys/artifacts" -v "$KUBECONFIG:/kubeconfig:ro" \
  -e KUBECONFIG=/kubeconfig -e VIBESYS_K8S_REGISTRY -e VIBESYS_K8S_BUILDER \
  rpc-policy-k8s-task /task/rpc-policy-k8s "$@" --project-root /candidate; }
mkdir -p out && chmod 777 out
run accuracy                                   # exit 0 PASS, 1 FAIL, 2 UNKNOWN; out/accuracy/report.txt
run benchmark --vs-output /vibesys/artifacts/result.json
.vibesys/tasks/rpc-policy-k8s/tests/mutants.sh rpc-policy-k8s-task /tmp/mutants   # base PASS, mutants FAIL
```

Locally, without a cluster, against `docker compose` (the task then seeds and measures what is
running and injects no faults; the application must be fresh, with nobody registered):

```sh
MODE=policy docker compose up -d --build       # frontend on localhost:5000
docker run --rm --init --network host -v "$PWD:/candidate:ro" -e SOCIALNET_TARGET=http://127.0.0.1:5000 \
  rpc-policy-k8s-task /task/rpc-policy-k8s accuracy --project-root /candidate --out /tmp/acc
docker compose down -v
```

`go test ./...` in `evaluator/` runs the unit tests: the oracle on a simulated application
(a correct history, and each violation it must catch: a missing newest post, a stale user
timeline, a doubled post, a post of a user the reader does not follow, a short page, an
altered text, a lost compose), the workload's streams (rotation of users, follow rule,
mentions) and the dataset, the manifest, the scope check.

## Mutants

`tests/mutants/*.patch` are policies that are wrong in ways an optimizing agent might try,
each a change of `policy/` only (in scope), using rpcstack hooks, kept out of the shipped
tree:

| Mutant | What it does | Why the check fails it |
| --- | --- | --- |
| `shed-home-write` | "Sheds" one WriteHomeTimeline in five with an empty OK before the handler runs | the compose succeeds but its post never reaches the followers' home timelines |
| `shed-user-write` | "Sheds" one WriteUserTimeline in five with an empty OK before the handler runs | the compose succeeds but its post never reaches the author's user timeline |
| `abort-after-compose` | A "deadline guard" failing one ComposePost child in three with DEADLINE_EXCEEDED after it was stored and delivered | 504s for posts that were made: unknown outcomes above 1% |

## Pinning and maintenance

- The evaluator pins are the Go SDK in `evaluator/go.mod` and the base image in `Dockerfile`;
  move them together.
- When the application changes (another commit of `crates/`, `config/`, the `Dockerfile`, a
  new rpcstack pin in `Cargo.toml`/`Cargo.lock`): `k8s/render.sh` and `/task/scope write
  --project-root . --out .vibesys/tasks/rpc-policy-k8s/scope.json` (or `go run ./cmd/scope
  write` from `evaluator/`), and check `internal/social` against the application (`go test`,
  then the base's accuracy check).
