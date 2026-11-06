# The rpc-policy-k8s task image: every job of the task runs in it, and so do its pods in the
# cluster (the probe, seed and phase pods of a benchmark sample or an accuracy check), which run
# this image rebuilt in-cluster from the task directory (/task).
#
# It holds vibesys-evaluator (the Python package from PyPI, whose platform wheel bundles the core
# vibesys-evaluator, loadgen and otelcapture), kubectl and buildctl, and the task's Go programs,
# built from its evaluator/ module (whose go.mod requires the Go SDK at the same release,
# v0.3.0-rc.3) into /task: rpc-policy-k8s (the benchmark and the accuracy check), oracle (judges
# a captured history offline), scope (writes or checks scope.json) and explore (the benchmark at
# other loads). The evaluator's own image (ghcr.io/uw-syfi/vibesys-evaluator-microservices) holds
# the same; it is not the base here because it is private, and the in-cluster rebuild of this
# image pulls without credentials (a 401 once BuildKit's cache no longer holds its layers).

# The one place the vibesys-evaluator release is pinned in this file (the PyPI version; the Go SDK
# in evaluator/go.mod is the same release).
ARG VIBESYS_EVALUATOR_VERSION=0.3.0rc3
ARG KUBECTL_VERSION=v1.34.0
ARG KUBECTL_SHA256=cfda68cba5848bc3b6c6135ae2f20ba2c78de20059f68789c090166d6abc3e2c

FROM golang:1.25.9-bookworm@sha256:298734aec230b5f3e8cee450ce6d7eccc39f1797ba548ee90d57e9803030c6c3 AS evaluator
ARG KUBECTL_VERSION
ARG KUBECTL_SHA256
ENV CGO_ENABLED=0 GOFLAGS=-trimpath GOTOOLCHAIN=local
RUN mkdir /out && curl -fsSL -o /out/kubectl "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/amd64/kubectl" \
 && echo "${KUBECTL_SHA256}  /out/kubectl" | sha256sum -c - \
 && chmod 0755 /out/kubectl
COPY evaluator/go.mod evaluator/go.sum /build/evaluator/
RUN cd /build/evaluator && go mod download
COPY evaluator/ /build/evaluator/
RUN cd /build/evaluator && go build -o /out/ ./cmd/rpc-policy-k8s ./cmd/oracle ./cmd/scope ./cmd/explore

FROM moby/buildkit:v0.22.0@sha256:832fa7aa1eb3deb56fa62ae933bfa42dad9a83ff4824dbbaf173b49c722b59d0 AS buildkit

FROM python:3.12.12-slim-bookworm@sha256:593bd06efe90efa80dc4eee3948be7c0fde4134606dd40d8dd8dbcade98e669c
ARG VIBESYS_EVALUATOR_VERSION
RUN pip install --no-cache-dir "vibesys-evaluator==${VIBESYS_EVALUATOR_VERSION}"
# The Go task binaries find loadgen (and the core) on the PATH.
RUN d="$(python -c 'import os, vibesys_evaluator as v; print(os.path.dirname(v.__file__))')/_bin" \
 && for b in vibesys-evaluator loadgen otelcapture; do ln -s "$d/$b" /usr/local/bin/$b; done
COPY --from=buildkit /usr/bin/buildctl /usr/local/bin/buildctl
# A job runs as the invoking user, whose HOME may not exist: kubectl and buildctl keep their
# caches under /tmp.
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 HOME=/tmp KUBECACHEDIR=/tmp/.kube/cache
COPY --from=evaluator /out/kubectl /usr/local/bin/kubectl
COPY . /task
# The binaries the image builds into /task: when vibesys-evaluator rebuilds this image in the
# cluster from /task, they are in the context (see .dockerignore) and must not be copied over
# the fresh ones.
COPY --from=evaluator /out/rpc-policy-k8s /out/oracle /out/scope /out/explore /task/
