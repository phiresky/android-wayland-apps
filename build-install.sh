#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
source .env

echo "==> cargo ndk build"
cargo ndk build "$@"

echo "==> gradlew installDebug"
cd android && ./gradlew installDebug
