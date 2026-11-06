#!/usr/bin/env bash
# Unit and regression tests in every policy mode. One mode at a time with
# explicit packages: see the note in scripts/check.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "== cargo test (baseline)"
cargo test --locked -p socialnet -p app-utils --features socialnet/baseline
echo "== cargo test (policy, with the policy crate)"
cargo test --locked -p socialnet -p app-utils -p socialnet-policy --features socialnet/policy
