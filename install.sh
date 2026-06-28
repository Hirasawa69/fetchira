#!/usr/bin/env bash
# Build fetchira, install the binary, and seed its home dir so MCP clients can launch it
# from anywhere. Re-runnable; never overwrites existing config/sessions.
set -euo pipefail
cd "$(dirname "$0")"

echo "==> building fetchira (release)…"
cargo build --release

BIN_DST="${BIN_DST:-$HOME/.local/bin}"
mkdir -p "$BIN_DST"
# Replace cleanly: overwriting a running/mapped binary corrupts its code signature, and macOS
# (Apple Silicon) then SIGKILLs it. Stop any running copy, remove, copy, re-sign ad-hoc.
pkill -9 -f "$BIN_DST/fetchira" 2>/dev/null || true
rm -f "$BIN_DST/fetchira"
cp target/release/fetchira "$BIN_DST/fetchira"
command -v codesign >/dev/null 2>&1 && codesign --force --sign - "$BIN_DST/fetchira" 2>/dev/null || true
echo "==> installed binary: $BIN_DST/fetchira"

HOME_DIR="${FETCHIRA_HOME:-${XDG_CONFIG_HOME:-$HOME/.config}/fetchira}"
mkdir -p "$HOME_DIR"

# Migrate an existing repo-local config/db (incl. captured web sessions) into the home dir.
for f in fetchira.toml .env usage.db usage.db-shm usage.db-wal; do
  if [ ! -e "$HOME_DIR/$f" ] && [ -e "$f" ]; then
    cp "$f" "$HOME_DIR/$f" && echo "    migrated $f -> $HOME_DIR/"
  fi
done
# Otherwise seed from the examples.
[ -e "$HOME_DIR/fetchira.toml" ] || { cp fetchira.toml.example "$HOME_DIR/fetchira.toml"; echo "    seeded fetchira.toml"; }
[ -e "$HOME_DIR/.env" ] || { cp .env.example "$HOME_DIR/.env"; echo "    seeded .env"; }

echo "==> fetchira home: $HOME_DIR"
echo "    register as MCP later:  claude mcp add fetchira -- $BIN_DST/fetchira"

# Launch the interactive setup TUI when run in a real terminal.
if [ -t 0 ] && [ -t 1 ]; then
  echo
  "$BIN_DST/fetchira" setup
else
  echo "    run \`$BIN_DST/fetchira setup\` to configure providers."
fi
