#!/bin/sh
# Install agent-pager from GitHub Releases, then run `agent-pager setup`.
#
#   curl -fsSL https://raw.githubusercontent.com/mpeyfuss/agent-pager/main/install.sh | sh
#
# Extra args go to `setup`:  ... | sh -s -- --claude-dir ~/.my-claude
# Env: AGENT_PAGER_VERSION=v0.1.0   AGENT_PAGER_BIN_DIR=~/bin   AGENT_PAGER_NO_SETUP=1
set -eu

REPO="${AGENT_PAGER_REPO:-mpeyfuss/agent-pager}"
VERSION="${AGENT_PAGER_VERSION:-latest}"
BIN_DIR="${AGENT_PAGER_BIN_DIR:-$HOME/.local/bin}"
ASSET=agent-pager-macos-universal.tar.gz

if [ "$(uname -s)" != Darwin ]; then
  echo "agent-pager only supports macOS" >&2
  exit 1
fi

if [ "$VERSION" = latest ]; then
  BASE="https://github.com/$REPO/releases/latest/download"
else
  BASE="https://github.com/$REPO/releases/download/$VERSION"
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "downloading $ASSET ($VERSION) from $REPO"
curl -fsSL "$BASE/$ASSET" -o "$TMP/$ASSET"
curl -fsSL "$BASE/$ASSET.sha256" -o "$TMP/$ASSET.sha256"
if ! (cd "$TMP" && shasum -a 256 -c "$ASSET.sha256" >/dev/null); then
  echo "checksum mismatch; aborting" >&2
  exit 1
fi

tar -xzf "$TMP/$ASSET" -C "$TMP"
mkdir -p "$BIN_DIR"
install -m 755 "$TMP/agent-pager" "$BIN_DIR/agent-pager"
xattr -d com.apple.quarantine "$BIN_DIR/agent-pager" 2>/dev/null || true
echo "installed $BIN_DIR/agent-pager ($("$BIN_DIR/agent-pager" --version))"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "note: $BIN_DIR is not on your PATH" ;;
esac

if [ -z "${AGENT_PAGER_NO_SETUP:-}" ]; then
  echo
  "$BIN_DIR/agent-pager" setup "$@"
fi
