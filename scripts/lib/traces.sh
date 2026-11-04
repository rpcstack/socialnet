#!/usr/bin/env bash
# Trace analysis shared by the smoke scripts.
#
#   check_traces <dir> <min-success-percent> [--expect-early-returns]
#
# Reads the load generator's r<rps>_<api>.csv files. Error column values:
#   /None          success within the SLO
#   /ClientMiss    completed, but after the SLO
#   /ClientTimeout client-side timeout
#   /EarlyReturn*  rejected by the policy's deadline/admission guards
#   anything else  an RPC error (transport failure, application error, ...)
#
# Fails on any RPC error ("transport errors" must be zero), on a success rate
# below the threshold, and, with --expect-early-returns, when the policy never
# rejected anything (used for the overload run in Masa mode).
check_traces() {
  local dir=$1 min=$2 expect_er=${3:-}
  local files
  files=$(ls "$dir"/r*_*.csv 2>/dev/null) || { echo "FAIL: no trace CSVs in $dir"; return 1; }
  # shellcheck disable=SC2086
  awk -F, -v min="$min" -v expect_er="$expect_er" '
    FNR == 1 { next }
    {
      e = $7; gsub(/^"/, "", e); total++
      if (e == "/None") ok++
      else if (e == "/ClientMiss") miss++
      else if (e == "/ClientTimeout") tmo++
      else if (e ~ /^\/EarlyReturn/) er++
      else { rpc++; if (rpc <= 5) sample[rpc] = e }
    }
    END {
      if (total == 0) { print "FAIL: traces are empty"; exit 1 }
      pct = 100 * ok / total
      printf "requests=%d ok=%d (%.1f%%) client_miss=%d client_timeout=%d early_return=%d rpc_errors=%d\n", total, ok, pct, miss, tmo, er, rpc
      for (i = 1; i <= rpc && i <= 5; i++) printf "  rpc error sample: %s\n", sample[i]
      bad = 0
      if (rpc > 0) { print "FAIL: transport/RPC errors present"; bad = 1 }
      if (pct < min) { printf "FAIL: success rate %.1f%% below %s%%\n", pct, min; bad = 1 }
      if (expect_er != "" && er == 0) { print "FAIL: overload produced no policy early returns"; bad = 1 }
      exit bad
    }' $files
}
