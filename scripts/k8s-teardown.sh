#!/usr/bin/env bash
# Remove the socialnet deployment; with --cluster also delete the kind cluster
# and registry (scripts/kind-down.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib/k8s.sh
kubectl --context "$(k8s_ctx)" delete namespace "$NS" --ignore-not-found --wait=true || true
if [ "${1:-}" = --cluster ]; then scripts/kind-down.sh; fi
