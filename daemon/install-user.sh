#!/bin/bash
# Build the broker and put it in the user's PATH. No root, no system unit:
# the overlay starts omassistantd on demand and it dies with the shell.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:$PATH"

# Build outside this checkout. When the plugin lives in
# ~/.config/omarchy/plugins/omassistant, Omarchy's shell watches that directory
# with `inotifywait -r` and reloads the plugin on any file change — and a reload
# closes the panel, so a build inside the plugin folder shuts the card mid-answer
# (thousands of files under target/ = thousands of reloads).
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/omassistant-build}"

cargo build --release
install -Dm755 "$CARGO_TARGET_DIR/release/omassistantd" "$HOME/.local/bin/omassistantd"
install -Dm755 "$CARGO_TARGET_DIR/release/omassistantctl" "$HOME/.local/bin/omassistantctl"
install -Dm755 "$CARGO_TARGET_DIR/release/omassistant-mcp" "$HOME/.local/bin/omassistant-mcp"
if ! command -v wlrctl >/dev/null; then
  echo "note: wlrctl is not installed; pointer control will be unavailable (AUR: wlrctl)"
fi
echo "installed: $(command -v omassistantd) $(command -v omassistantctl)"
