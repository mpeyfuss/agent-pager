#!/bin/sh
# Build a universal (arm64 + x86_64) release tarball into dist/.
# Needs: rustup target add aarch64-apple-darwin x86_64-apple-darwin
set -eu
cd "$(dirname "$0")/.."

ASSET=agent-pager-macos-universal.tar.gz
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --locked --target "$target"
done

rm -rf dist && mkdir -p dist
lipo -create -output dist/agent-pager \
  target/aarch64-apple-darwin/release/agent-pager \
  target/x86_64-apple-darwin/release/agent-pager
codesign --force --sign - dist/agent-pager

tar -C dist -czf "dist/$ASSET" agent-pager
rm dist/agent-pager
(cd dist && shasum -a 256 "$ASSET" > "$ASSET.sha256")
echo "built dist/$ASSET"
