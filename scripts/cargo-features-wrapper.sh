#!/usr/bin/env bash
set -e

cargo features prune
taplo fmt Cargo.toml
