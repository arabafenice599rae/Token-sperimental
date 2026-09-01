#!/bin/bash
# SessionStart hook: bootstraps the Solana/Rust toolchain for Claude Code on the web.
# The container is ephemeral, so anything installed here is rebuilt on every new session.
set -euo pipefail

# Only bootstrap remote (web) sessions; local machines keep their own setup.
if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

# Environment-config values are passed verbatim, without shell expansion. A literal
# "$PATH" or "$HOME" left in one of them would otherwise take out every system binary.
case "${PATH:-}" in
  *'$PATH'* | *'$HOME'*)
    PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    export PATH
    ;;
esac

for var in RUSTUP_HOME CARGO_HOME ANCHOR_WALLET; do
  value="${!var:-}"
  case "$value" in
    *'$HOME'*)
      expanded="${value//\$HOME/$HOME}"
      export "$var=$expanded"
      [ -n "${CLAUDE_ENV_FILE:-}" ] && echo "export $var=\"$expanded\"" >> "$CLAUDE_ENV_FILE"
      ;;
  esac
done

SOLANA_BIN="$HOME/.local/share/solana/install/active_release/bin"
KEYPAIR="${ANCHOR_WALLET:-$HOME/.config/solana/id.json}"
CLUSTER="${SOLANA_CLUSTER:-localhost}"

# 1. Native build dependencies for Solana program compilation.
missing=""
for pkg in build-essential pkg-config libudev-dev libssl-dev; do
  dpkg -s "$pkg" >/dev/null 2>&1 || missing="$missing $pkg"
done
if [ -n "$missing" ]; then
  # Some third-party PPAs are blocked by the egress policy; main archives still refresh.
  sudo apt-get update -qq || true
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $missing
fi

# 2. Solana CLI (ships its own SBF platform-tools, so no GitHub access needed).
if [ ! -x "$SOLANA_BIN/solana" ]; then
  sh -c "$(curl -sSfL https://release.anza.xyz/stable/install)"
fi
export PATH="$SOLANA_BIN:$PATH"
if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
  echo "export PATH=\"$SOLANA_BIN:\$PATH\"" >> "$CLAUDE_ENV_FILE"
fi

# 3. Dev keypair. Never overwrite one that already exists.
if [ ! -f "$KEYPAIR" ]; then
  mkdir -p "$(dirname "$KEYPAIR")"
  solana-keygen new -o "$KEYPAIR" --no-bip39-passphrase --silent
fi

# 4. Cluster and default signer.
solana config set --url "$CLUSTER" --keypair "$KEYPAIR" >/dev/null

# 5. Warm the cargo registry once the repo grows a manifest.
if [ -f "${CLAUDE_PROJECT_DIR:-.}/Cargo.toml" ]; then
  (cd "${CLAUDE_PROJECT_DIR:-.}" && cargo fetch --quiet) || true
fi

echo "Solana $(solana --version | awk '{print $2}') ready | cluster: $CLUSTER | pubkey: $(solana address)"
