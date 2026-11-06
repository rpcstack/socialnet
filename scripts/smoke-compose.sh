#!/usr/bin/env bash
# Compose smoke test for one mode.
#
#   scripts/smoke-compose.sh baseline|policy
#
# 1. build and start the stack, wait until ComposePost works end to end
# (SKIP_BUILD=1 reuses an existing rpcstack-socialnet:$MODE image.)
# 2. stack probe: control ComposePosts and one ordinary request to every service
# 3. HTTP API check: every route of the HTTP frontend answers with the status and
#    JSON shape of DeathStarBench's nginx/lua gateway
# 4. load run (config/gen_config.json): zero RPC errors, success >= 95%
# The initial no-op `policy` mode is judged exactly like `baseline`.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib/traces.sh

MODE=${1:?usage: smoke-compose.sh baseline|policy}
case "$MODE" in baseline|policy) ;; *) echo "mode must be baseline or policy" >&2; exit 2;; esac
export MODE
MIN_SUCCESS=${MIN_SUCCESS:-95}
PROJECT=rpcstack-socialnet-$MODE
DC="docker compose"
KEEP=${KEEP:-0}

cleanup() {
  status=$?
  if [ $status -ne 0 ]; then
    echo "== smoke FAILED; last service logs"; $DC logs --tail 20 2>&1 | tail -n 120 || true
  fi
  if [ "$KEEP" != 1 ]; then $DC down -v --remove-orphans >/dev/null 2>&1 || true; fi
  exit $status
}
trap cleanup EXIT

echo "== [$MODE] build and start ($PROJECT)"
rm -rf "out/$MODE"; mkdir -p "out/$MODE"
# Every service shares one image: build it once (parallel builds of the same tag
# collide), then start the stack from it.
if [ "${SKIP_BUILD:-0}" != 1 ]; then $DC build frontend-service; fi
$DC up -d --scale loadgen=0

TARGETS=""
for t in compose_post:compose-post register_user:register-user user:user unique_id:unique-id \
         media:media text:text user_mention:user-mention url_shorten:url-shorten \
         post_storage:post-storage user_timeline:user-timeline home_timeline:home-timeline \
         social_graph:social-graph; do
  case "$t" in
    text:*) host=text-service-1;;
    *) host=${t#*:}-service-1;;
  esac
  TARGETS="$TARGETS --target ${t%%:*}=http://$host:8080"
done

probe_run() {
  # shellcheck disable=SC2086
  $DC run --rm --no-deps -T loadgen probe --wait-secs 240 --control 20 $TARGETS
}

# Cold start: services restart once while their databases start. Retry the
# probe a few times until the stack settles; a run only passes when ALL of its
# checks (20/20 control requests, 13/13 services) pass in that one run.
echo "== [$MODE] stack probe"
probe_ok=0
for attempt in 1 2 3 4 5 6; do
  if probe_run >/tmp/probe-$$.log 2>&1; then probe_ok=1; break; fi
  echo "probe attempt $attempt failed:"; { grep -E "control|PROBE|FAIL|->" /tmp/probe-$$.log || true; } | tail -n 8
  sleep 10
done
cat /tmp/probe-$$.log; rm -f /tmp/probe-$$.log
[ "$probe_ok" = 1 ] || { echo "FAIL: stack probe never passed"; exit 1; }

echo "== [$MODE] HTTP API check"
$DC run --rm --no-deps -T loadgen http-check --wait-secs 240 \
  | tee "out/$MODE/http-check.txt"
grep -q "HTTP CHECK PASS" "out/$MODE/http-check.txt" || { echo "FAIL: HTTP API check"; exit 1; }

run_load() {  # run_load <gen-config-file>
  rm -f "out/$MODE"/r*_*.csv
  GEN_CONFIG=$1 $DC run --rm --no-deps -T loadgen >"out/$MODE/loadgen.stdout" 2>&1 \
    || { tail -n 30 "out/$MODE/loadgen.stdout"; return 1; }
}

echo "== [$MODE] load run (config/gen_config.json)"
run_load gen_config.json
check_traces "out/$MODE" "$MIN_SUCCESS"
echo "SMOKE PASS (compose, $MODE)"
