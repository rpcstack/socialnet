#!/usr/bin/env bash
# Compose smoke test for one mode.
#
#   scripts/smoke-compose.sh baseline|masa
#
# 1. build and start the stack, wait until ComposePost works end to end
# (SKIP_BUILD=1 reuses an existing rpcstack-socialnet:$MODE image.)
# 2. policy-live probe: expired-deadline RPCs to every service must be rejected
#    by Masa's hooks in masa mode and must NOT be in baseline mode
# 3. load run (config/gen_config.json): zero RPC errors, success >= 95%
# 4. masa mode only: short overload run must trigger early returns, then a
#    normal run must recover to >= 95% success
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/lib/traces.sh

MODE=${1:?usage: smoke-compose.sh baseline|masa}
case "$MODE" in baseline|masa) ;; *) echo "mode must be baseline or masa" >&2; exit 2;; esac
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
if [ "${SKIP_BUILD:-0}" = 1 ]; then build=""; else build="--build"; fi
$DC up $build -d --scale loadgen=0

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

# Services that raced their databases at startup may still be settling: warm
# the stack up (result ignored) so the assertions below see a settled system.
echo "== [$MODE] warm-up"
# shellcheck disable=SC2086
$DC run --rm --no-deps -T loadgen probe --expect "$MODE" --wait-secs 240 --control 20 $TARGETS >/dev/null 2>&1 || true

echo "== [$MODE] policy-live probe (expect $MODE)"
# shellcheck disable=SC2086
$DC run --rm --no-deps -T loadgen probe --expect "$MODE" \
  --wait-secs 240 --control 20 $TARGETS

if [ "$MODE" = baseline ]; then
  echo "== [baseline] negative check: the Masa policy-live expectation must FAIL here"
  # shellcheck disable=SC2086
  if $DC run --rm --no-deps -T loadgen probe --expect masa \
       --control 1 $TARGETS >/tmp/neg-$$.log 2>&1; then
    cat /tmp/neg-$$.log; echo "FAIL: policy-live check passed in baseline"; exit 1
  fi
  grep -E "policy-live|PROBE" /tmp/neg-$$.log; rm -f /tmp/neg-$$.log
  echo "policy-live check failed in baseline, as required"
fi

run_load() {  # run_load <gen-config-file>
  rm -f "out/$MODE"/r*_*.csv
  GEN_CONFIG=$1 $DC run --rm --no-deps -T loadgen >"out/$MODE/loadgen.stdout" 2>&1 \
    || { tail -n 30 "out/$MODE/loadgen.stdout"; return 1; }
}

echo "== [$MODE] load run (config/gen_config.json)"
run_load gen_config.json
check_traces "out/$MODE" "$MIN_SUCCESS"

if [ "$MODE" = masa ]; then
  echo "== [masa] overload run (config/gen_config.overload.json)"
  run_load gen_config.overload.json
  # Overload may produce client timeouts and misses; what matters is that the
  # policy shed load (early returns) and that there were no transport errors.
  OVERLOAD=1 check_traces "out/$MODE" 0 --expect-early-returns
  echo "== [masa] recovery run (config/gen_config.json)"
  run_load gen_config.json
  check_traces "out/$MODE" "$MIN_SUCCESS"
fi
echo "SMOKE PASS (compose, $MODE)"
