#!/usr/bin/env bash
# Remove the kind cluster and the local registry created by k8s-up.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib/k8s.sh
k8s_down
