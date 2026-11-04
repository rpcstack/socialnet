#!/usr/bin/env bash
# One-command Kubernetes deploy: kind cluster + local registry, local image
# build, the full application and the load generator.
#
#   scripts/k8s-up.sh [baseline|masa]       # MODE can also be set in the environment
#   LOADGEN=0 scripts/k8s-up.sh masa        # application only
#
# Follow the load generator with:  kubectl -n socialnet logs -f job/loadgen
# Tear everything down with:       scripts/k8s-down.sh
set -euo pipefail
cd "$(dirname "$0")/.."
MODE=${1:-${MODE:-baseline}}
case "$MODE" in baseline|masa) ;; *) echo "mode must be baseline or masa" >&2; exit 2;; esac
export MODE
. scripts/lib/k8s.sh

k8s_cluster_up
k8s_build_push
k8s_deploy
if [ "${LOADGEN:-1}" = 1 ]; then k8s_loadgen gen_config.json; fi
echo "socialnet ($MODE) is up in kind cluster $CLUSTER, namespace $NS"
echo "  kubectl --context kind-$CLUSTER -n $NS get pods"
echo "  kubectl --context kind-$CLUSTER -n $NS logs -f job/loadgen"
