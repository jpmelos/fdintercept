#!/bin/bash
set -e

# Change to the parent directory of this script.
cd "$(dirname "${BASH_SOURCE[0]}")/.."

docker build -t fdintercept-coverage -f scripts/Dockerfile .
docker run --rm \
    --user "$(id -u):$(id -g)" \
    -v "$(pwd)":/fdintercept \
    fdintercept-coverage cargo tarpaulin --out Html

open tarpaulin-report.html
