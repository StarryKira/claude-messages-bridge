#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ! -f .env ]]; then
  python3 scripts/init-env.py
fi
set -a
source .env
set +a
if [[ ! -x target/release/claude-messages-bridge || ! -f web/dist/index.html ]]; then
  echo 'Build the service first: bash scripts/build.sh' >&2
  exit 1
fi
exec ./target/release/claude-messages-bridge
