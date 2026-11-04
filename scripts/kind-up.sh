#!/usr/bin/env bash
# Create a kind cluster wired to a local registry. Idempotent.
#
#   scripts/kind-up.sh
#
# Environment: CLUSTER (default rpcstack-socialnet), REGISTRY (registry
# container, default rpcstack-socialnet-registry), REG_PORT (host port, default
# the registry's existing port or the first free one from 5601).
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib/k8s.sh
k8s_cluster_up
echo "kind cluster $(k8s_ctx) ready; registry localhost:$(k8s_reg_port)"
