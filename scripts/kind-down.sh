#!/usr/bin/env bash
# Delete the kind cluster and local registry created by scripts/kind-up.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib/k8s.sh
k8s_down
echo "deleted kind cluster $CLUSTER and registry $REGISTRY"
