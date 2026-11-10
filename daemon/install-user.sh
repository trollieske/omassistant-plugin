#!/bin/bash
# Build the broker and put it in the user's PATH. No root, no system unit:
# the overlay starts omassistantd on demand and it dies with the shell.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release
install -Dm755 target/release/omassistantd "$HOME/.local/bin/omassistantd"
install -Dm755 target/release/omassistantctl "$HOME/.local/bin/omassistantctl"
install -Dm755 target/release/omassistant-mcp "$HOME/.local/bin/omassistant-mcp"
if ! command -v wlrctl >/dev/null; then
  echo "note: wlrctl is not installed; pointer control will be unavailable (AUR: wlrctl)"
fi
echo "installed: $(command -v omassistantd) $(command -v omassistantctl)"
