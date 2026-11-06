#!/usr/bin/env bash
# Build the socialnet image for MODE, push it to the local registry and deploy
# k8s/base (baseline) or k8s/overlays/<mode>. Expects scripts/kind-up.sh first.
#
#   scripts/k8s-deploy.sh [baseline|policy]       # MODE may also be in the environment
#   LOADGEN=1 scripts/k8s-deploy.sh policy        # also start the load generator Job
#   SKIP_BUILD=1 scripts/k8s-deploy.sh policy     # reuse the pushed image
#   PREBUILT=1 scripts/k8s-deploy.sh policy       # push the local rpcstack-socialnet:policy image
#
# Environment (see scripts/lib/k8s.sh): KIND_CLUSTER, REG_PORT (registry
# localhost:<port>), SOCIALNET_NAMESPACE, SOCIALNET_IMAGE_NAME, BUILDKIT_ADDR.
#
# Follow the load generator with:  kubectl -n $NS logs -f job/loadgen
set -euo pipefail
cd "$(dirname "$0")/.."
MODE=${1:-${MODE:-baseline}}
case "$MODE" in baseline|policy) ;; *) echo "usage: $0 [baseline|policy]" >&2; exit 2;; esac
export MODE
. scripts/lib/k8s.sh

[ "${SKIP_BUILD:-0}" = 1 ] || k8s_build_push
k8s_deploy
if [ "${LOADGEN:-0}" = 1 ]; then k8s_loadgen gen_config.json; fi
echo "socialnet ($MODE) deployed to namespace $NS in kind cluster $CLUSTER"
echo "  kubectl --context $(k8s_ctx) -n $NS get pods"
