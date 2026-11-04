#!/usr/bin/env bash
# Kubernetes (kind) smoke test for one mode; same assertions as smoke-compose.sh.
#
#   scripts/smoke-k8s.sh baseline|masa
#
# Set KEEP=1 to leave the cluster up afterwards.
set -euo pipefail
cd "$(dirname "$0")/.."
MODE=${1:?usage: smoke-k8s.sh baseline|masa}
case "$MODE" in baseline|masa) ;; *) echo "mode must be baseline or masa" >&2; exit 2;; esac
export MODE
. scripts/lib/k8s.sh
. scripts/lib/traces.sh
MIN_SUCCESS=${MIN_SUCCESS:-95}
OUT=out/k8s-$MODE

cleanup() {
  status=$?
  if [ $status -ne 0 ]; then
    echo "== smoke FAILED; pod state"; kc get pods 2>&1 | tail -n 40 || true
    kc logs job/loadgen --tail 20 2>&1 || true
  fi
  if [ "${KEEP:-0}" != 1 ]; then k8s_down >/dev/null 2>&1 || true; fi
  exit $status
}
trap cleanup EXIT

k8s_cluster_up
k8s_build_push
k8s_deploy

TARGETS=""
for t in compose_post:compose-post register_user:register-user user:user unique_id:unique-id \
         media:media text:text user_mention:user-mention url_shorten:url-shorten \
         post_storage:post-storage user_timeline:user-timeline home_timeline:home-timeline \
         social_graph:social-graph; do
  TARGETS="$TARGETS --target ${t%%:*}=http://${t#*:}-service-1:8080"
done

echo "== [$MODE] policy-live probe (expect $MODE)"
# shellcheck disable=SC2086
k8s_probe "$MODE" --wait-secs 240 --control 20 $TARGETS

run_load() { k8s_loadgen "$1" "$OUT"; }

echo "== [$MODE] load run (config/gen_config.json)"
run_load gen_config.json
check_traces "$OUT" "$MIN_SUCCESS"

if [ "$MODE" = masa ]; then
  echo "== [masa] overload run (config/gen_config.overload.json)"
  run_load gen_config.overload.json
  check_traces "$OUT" 0 --expect-early-returns
  echo "== [masa] recovery run (config/gen_config.json)"
  run_load gen_config.json
  check_traces "$OUT" "$MIN_SUCCESS"
fi
echo "SMOKE PASS (k8s, $MODE)"
