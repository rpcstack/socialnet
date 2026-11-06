# Social network on rpcstack: an RPC policy for throughput and p99 latency

## Goal

Write the RPC policy of the social network application (DeathStarBench's social network,
ported to Rust on the rpcstack framework) that raises its throughput and lowers its tail
latency. Both are measured on every sample, against a Kubernetes cluster, with the same request
mix:

| Objective | Direction | How it is measured |
| --- | --- | --- |
| `operations_per_second` | max | Closed loop: 32 concurrent clients, each sending its next request when the last one answered; 2 trials of 60 s after a 15 s warmup; the median. |
| `latency_ms.p99` | min | Open loop: 800 requests per second, evenly spaced, whatever the answers; 3 trials of 60 s after a 10 s warmup; the median of their p99 latencies (from each request's scheduled send time to its answer). |

Together they form a Pareto frontier: raising throughput and lowering the p99 is a win;
trading one for the other is a tradeoff. The base (the no-op policy) scores about
3,300 ops/s and 54 ms.

The mix is the DeathStarBench social network's, sent to the frontend's HTTP API (port 5000)
over a seeded graph of 1,000 users (24,000 follow edges; popular users have hundreds of
followers): 50% user timeline reads (`GET /wrk2-api/user-timeline/read`, a page of 10 posts of
a user drawn by popularity), 40% home timeline reads (`GET /wrk2-api/home-timeline/read`, a page
of a user drawn by activity), 10% composes (`POST /wrk2-api/post/compose`: a few words, a
mention of a followee 20% of the time, a URL 15%, media 5%, followed by a read of the author's
own user timeline), and 0.5% follows and 0.5% unfollows. A compose calls compose-post, which
calls the text, user-mention, URL-shorten, media, unique-id and user services, then stores the
post and writes the author's user timeline and the home timelines of every follower and of the
mentioned users, all before it answers.

## What you edit

Only `policy/`: the run-queue scheduler (`policy/queue/queue.rs` and files next to it) and
the module stack that runs in the hooks of every gRPC client and server stub
(`policy/src/stack.rs` and the modules you add under `policy/src/`), plus `policy/Cargo.toml`
for the dependencies your modules need. `policy/README.md` explains the two slots, how to
build and test a change, and the invariants a policy keeps; the `rpcstack/` submodule holds
the framework source and its published policies (masa, breakwater, dagor, rajomon,
tailclipper, c3, hedged, ...), `docs/PAPERS.md` and `docs/VIBESYS.md`.

Everything else that goes into the image is the task's, and a candidate that changes it is
refused before anything is built (the benchmark records an error, the accuracy check fails):
the services (`crates/`), `Cargo.toml` and the rpcstack pin, `.cargo/`, `config/` (the
per-route SLOs among it), `docker/`, `rust-toolchain.toml`, `Dockerfile` and
`.dockerignore`. `Cargo.lock` may gain the crates your policy's new dependencies need, and
the `socialnet-policy` entry its new dependency list; no other entry may change. The policy
crate may not have a build script (`policy/build.rs`, `package.build`) or be a procedural
macro: code that runs at build time could rewrite the application. The task's
`scope.json` pins all of it.

The task builds one image from the root `Dockerfile` (its default `MODE` is `policy`: the
`policy/` directory compiled in), and deploys the repository's manifests (`k8s/base`, every
service with one replica, no resource limits) into a namespace of its own, then registers the
users and the follow graph through the HTTP API. The deployment, the replica counts, the
databases and caches, the seed, the request mix, the loads and the oracle are fixed.

## Validity

A phase counts only if no request in its window failed (any status but 200, a timeout or a
transport error), every operation of the mix completed at least once, and the open loop
offered at least 95% of its rate. A policy may reorder, delay, prioritise or route work, but
not shed it by failing requests: a rejected or aborted request (`503`, `504`) makes the sample
an error. The application enforces no deadline of its own; requests carry their route's SLO
(200 ms, `config/socialnet.json` `frontend.sloMs`) as `grpc-timeout`, which a module may read.

## Correctness

The accuracy check deploys the candidate as a sample does and sends a different mix at 300
requests per second, in four phases of 45 s, with every answer recorded: posts, follows and
unfollows, and reads of the user and home timelines, all by 40 popular users, whose operations
come round in turn (so one user's requests never overlap), plus a few invalid requests (no
text, no followee, a non-numeric user, an empty or negative window), which the application must
refuse or answer as it does. Between the phases it restarts services, empties the caches (the
timeline, social graph, user and post-storage caches and the user-mention cache) in place, and
restarts every service and the Redis pods at once. The task's oracle then judges the whole
history and the check passes only if:

- every user timeline shows exactly the posts its user composed, newest first, window by
  window, with no post lost, doubled or invented, across the restarts and flushes;
- every home timeline shows exactly the posts of the users its reader followed when they were
  composed (the seeded graph and the reader's follow changes; an unfollow does not take posts
  back) and of those who mentioned the reader, newest first, window by window: a post
  acknowledged before a read started is there (reads are not allowed to be stale), and the
  reader's own posts are not (the application does not write an author's own home timeline);
- every post is exactly what was composed (text with the mention and the URL rewritten as the
  application does, the creator, the media, the mentions resolved against the application's
  table of a million fake users), with one timestamp whenever it is served;
- the invalid requests get the application's refusals;
- at most 1% of the answers are lost or 5xx (their outcome is unknown); every other answer is
  what the application gives, with the frontend's body.

## Rules

- Do not fabricate, reuse or cache answers that the application would compute differently:
  a cached timeline, an empty or default reply in place of a real one, or a success for
  work that failed. A response must come from the handler that owns it.
- Do not duplicate a non-idempotent call (a compose) or report failure for one that took
  effect; retries and hedging are for calls that are safe to repeat.
- Do not detect the benchmark or the accuracy check (by request pattern, users, namespace or
  environment) and behave differently.
- Do not depend on anything outside the policy crate and the deployment: no reading or
  writing of the databases or caches from a module, no files, no network calls of a module's
  own beyond the application's RPCs.
- Keep the invariants of `policy/README.md` (hooks stay wired, the queue marker, the
  run-queue contract, light and locked dependencies).

## Running it

`.vibesys/tasks/rpc-policy-k8s/README.md` ("Run it") shows how to run the benchmark and the
accuracy check on a Kubernetes cluster, and against a local `docker compose` deployment.
