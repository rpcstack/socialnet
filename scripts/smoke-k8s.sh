#!/usr/bin/env bash
# Kubernetes (kind) smoke test for one mode; same assertions as smoke-compose.sh.
# Creates the cluster (scripts/kind-up.sh) and deploys (scripts/k8s-deploy.sh).
#
#   scripts/smoke-k8s.sh baseline|policy
#
# Checks: stack probe, HTTP API check (every route of the HTTP frontend), load
# run. The initial no-op `policy` mode is judged exactly like `baseline`.
#
# Environment (see scripts/lib/k8s.sh): KIND_CLUSTER, REG_PORT,
# SOCIALNET_NAMESPACE, SOCIALNET_IMAGE_NAME, BUILDKIT_ADDR. SKIP_KIND_UP=1 uses
# an existing cluster and registry instead of creating them (scripts/kind-up.sh);
# then only the namespace is deleted afterwards. Otherwise the cluster is
# deleted afterwards too; KEEP=1 leaves everything up.
set -euo pipefail
cd "$(dirname "$0")/.."
MODE=${1:?usage: smoke-k8s.sh baseline|policy}
case "$MODE" in baseline|policy) ;; *) echo "mode must be baseline or policy" >&2; exit 2;; esac
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
  if [ "${KEEP:-0}" != 1 ]; then
    if [ "${SKIP_KIND_UP:-0}" = 1 ]; then
      kubectl --context "$(k8s_ctx)" delete namespace "$NS" --ignore-not-found --wait=true >/dev/null 2>&1 || true
    else
      k8s_down >/dev/null 2>&1 || true
    fi
  fi
  exit $status
}
trap cleanup EXIT

[ "${SKIP_KIND_UP:-0}" = 1 ] || scripts/kind-up.sh
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
  k8s_probe --frontend http://frontend-service:8080 --wait-secs 240 --control 20 $TARGETS
}

# Cold start: services restart once while their databases start. Retry the
# probe a few times until the stack settles; a run only passes when ALL of its
# checks (20/20 control requests, 13/13 services) pass in that one run.
echo "== [$MODE] stack probe"
probe_ok=0
for attempt in 1 2 3 4 5 6; do
  if probe_run >/tmp/probe-$$.log 2>&1; then probe_ok=1; break; fi
  echo "probe attempt $attempt failed:"; grep -E "control|PROBE|FAIL|->" /tmp/probe-$$.log | tail -n 8
  sleep 10
done
cat /tmp/probe-$$.log; rm -f /tmp/probe-$$.log
[ "$probe_ok" = 1 ] || { echo "FAIL: stack probe never passed"; exit 1; }

echo "== [$MODE] HTTP API check (every route of the HTTP frontend)"
mkdir -p "$OUT"
k8s_run http-check http-check --wait-secs 240 | tee "$OUT/http-check.txt"
grep -q "HTTP CHECK PASS" "$OUT/http-check.txt" || { echo "FAIL: HTTP API check"; exit 1; }

run_load() { k8s_loadgen "$1" "$OUT"; }

echo "== [$MODE] load run (config/gen_config.json)"
run_load gen_config.json
check_traces "$OUT" "$MIN_SUCCESS"

echo "SMOKE PASS (k8s, $MODE)"
