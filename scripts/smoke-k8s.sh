#!/usr/bin/env bash
# Kubernetes (kind) smoke test for one mode; same assertions as smoke-compose.sh.
# Creates the cluster (scripts/kind-up.sh) and deploys (scripts/k8s-deploy.sh).
#
#   scripts/smoke-k8s.sh baseline|masa
#
# Set KEEP=1 to leave the cluster up afterwards (otherwise it is deleted).
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

scripts/kind-up.sh
scripts/k8s-deploy.sh "$MODE"

TARGETS=""
for t in compose_post:compose-post register_user:register-user user:user unique_id:unique-id \
         media:media text:text user_mention:user-mention url_shorten:url-shorten \
         post_storage:post-storage user_timeline:user-timeline home_timeline:home-timeline \
         social_graph:social-graph; do
  TARGETS="$TARGETS --target ${t%%:*}=http://${t#*:}-service-1:8080"
done

probe_run() {
  # shellcheck disable=SC2086
  k8s_probe "$MODE" --wait-secs 240 --control 20 $TARGETS
}

# Cold start: services restart once while their databases start, and on a slow
# machine Masa's admission control can reject the first control requests. Retry
# the probe a few times until the stack settles; a run only passes when ALL of
# its checks (20/20 control requests, 13/13 services) pass in that one run.
echo "== [$MODE] policy-live probe (expect $MODE)"
probe_ok=0
for attempt in 1 2 3 4 5 6; do
  if probe_run >/tmp/probe-$$.log 2>&1; then probe_ok=1; break; fi
  echo "probe attempt $attempt failed:"; grep -E "control|policy-live|PROBE|FAIL" /tmp/probe-$$.log | tail -n 6
  sleep 10
done
cat /tmp/probe-$$.log; rm -f /tmp/probe-$$.log
[ "$probe_ok" = 1 ] || { echo "FAIL: policy-live probe never passed"; exit 1; }

if [ "$MODE" = baseline ]; then
  echo "== [baseline] negative check: the Masa policy-live expectation must FAIL here"
  # shellcheck disable=SC2086
  if k8s_probe masa --control 1 $TARGETS >/tmp/neg-$$.log 2>&1; then
    cat /tmp/neg-$$.log; echo "FAIL: policy-live check passed in baseline"; exit 1
  fi
  grep -E "policy-live|PROBE" /tmp/neg-$$.log; rm -f /tmp/neg-$$.log
  echo "policy-live check failed in baseline, as required"
fi

run_load() { k8s_loadgen "$1" "$OUT"; }

echo "== [$MODE] load run (config/gen_config.json)"
run_load gen_config.json
check_traces "$OUT" "$MIN_SUCCESS"

if [ "$MODE" = masa ]; then
  echo "== [masa] overload run (config/gen_config.overload.json)"
  run_load gen_config.overload.json
  OVERLOAD=1 check_traces "$OUT" 0 --expect-early-returns
  echo "== [masa] recovery run (config/gen_config.json)"
  run_load gen_config.json
  check_traces "$OUT" "$MIN_SUCCESS"
fi
echo "SMOKE PASS (k8s, $MODE)"
