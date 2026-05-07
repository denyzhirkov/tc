#!/usr/bin/env bash
set -euo pipefail

DIST_DIR="dist"

echo "Building tc (release)..."
cargo build --release

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

cp target/release/tc-server "$DIST_DIR/"
cp target/release/tc-client "$DIST_DIR/"

echo ""
echo "Done! Binaries in $DIST_DIR/:"
ls -lh "$DIST_DIR/"
