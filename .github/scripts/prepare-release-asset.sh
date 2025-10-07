#!/bin/bash

# ---
# File Path: .github/scripts/prepare-release-asset.sh
#
# Script to prepare a release asset for a Rust project.
#
# This script reads configuration from environment variables, builds the
# archive (zip or tar.gz), and prints the final archive name to stdout.
# All informational logs are printed to stderr.
# ---

# Exit immediately if a command exits with a non-zero status.
set -e

# --- Input Validation ---
if [[ -z "$TAG_NAME" || -z "$TARGET" || -z "$OS" || -z "$EXT" ]]; then
  # Error messages should go to stderr
  echo "Error: Missing required environment variables (TAG_NAME, TARGET, OS, EXT)." >&2
  exit 1
fi

# --- Variable Preparation ---
VERSION=$(echo "$TAG_NAME" | sed 's/^v//')
BINARY_NAME=$(grep '^name =' Cargo.toml | head -1 | sed 's/name = "\(.*\)"/\1/' | tr '_' '-')

if [[ -z "$BINARY_NAME" ]]; then
  echo "Error: Could not determine binary name from Cargo.toml." >&2
  exit 1
fi

BIN_DIR="target/${TARGET}/release"
SOURCE_NAME="$BINARY_NAME"
[[ "$OS" == "windows-latest" ]] && SOURCE_NAME="$BINARY_NAME.exe"
SOURCE_PATH="$BIN_DIR/$SOURCE_NAME"

if [[ ! -f "$SOURCE_PATH" ]]; then
  echo "Error: Binary not found at $SOURCE_PATH" >&2
  exit 1
fi

# --- Strip Binary (if applicable) ---
if [[ -n "$STRIP_CMD" ]]; then
  # Log messages should go to stderr
  echo "Stripping binary at $SOURCE_PATH with command: $STRIP_CMD" >&2
  $STRIP_CMD "$SOURCE_PATH" || echo "Warning: Strip command failed for $SOURCE_PATH" >&2
fi

# --- Create Archive ---
ARCHIVE_NAME="${BINARY_NAME}-v${VERSION}-${TARGET}${EXT}"
# Log messages should go to stderr
echo "Creating archive: $ARCHIVE_NAME" >&2

# Change to the binary's directory to avoid including parent folders in the archive
cd "$BIN_DIR"

if [[ "$EXT" == ".zip" ]]; then
  7z a -bso0 "../../..//$ARCHIVE_NAME" "$SOURCE_NAME"
else
  tar -czf "../../../$ARCHIVE_NAME" "$SOURCE_NAME" > /dev/null
fi

# Return to the project root directory
cd ../../..

# --- Final Output ---
# This is the ONLY line that prints to stdout. It's the "data" output.
echo "$ARCHIVE_NAME"