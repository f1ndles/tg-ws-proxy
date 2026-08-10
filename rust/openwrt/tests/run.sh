#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
for test_file in "$DIR"/test-*.sh; do
    printf '==> %s\n' "$(basename "$test_file")"
    bash "$test_file"
done
