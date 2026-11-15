#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cd "$project_dir"

cargo build --release
install -Dm755 "target/release/lan-bridge" "$HOME/.local/bin/lan-bridge"
mkdir -p "$HOME/.local/share/lan-bridge/shared-files"

echo "Installed: $HOME/.local/bin/lan-bridge"
echo "Data: $HOME/.local/share/lan-bridge"
echo "Start: lan-bridge serve"
