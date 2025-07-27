#!/usr/bin/env bash
set -e
trap 'echo "Exit status $? at line $LINENO from: $BASH_COMMAND"' ERR

cargo features prune
taplo fmt Cargo.toml
