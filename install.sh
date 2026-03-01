#!/bin/sh
# vclaw installer — downloads the latest release binary for your platform.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/MadHouseLabs/vclaw/master/install.sh | sh

set -e

REPO="MadHouseLabs/vclaw"

# --- Detect OS and architecture ---

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)  os="unknown-linux-gnu" ;;
    Darwin) os="apple-darwin" ;;
    *)
        echo "Error: Unsupported OS: $OS"
        echo "Windows users: download the binary manually from"
        echo "  https://github.com/$REPO/releases/latest"
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64|amd64)   arch="x86_64" ;;
    arm64|aarch64)   arch="aarch64" ;;
    *)
        echo "Error: Unsupported architecture: $ARCH"
        echo "Pre-built binaries are available for x86_64 and aarch64."
        echo "You can build from source: cargo install --path ."
        exit 1
        ;;
esac

TARGET="${arch}-${os}"
BINARY="vclaw-${TARGET}"

echo "Detected platform: ${TARGET}"

# --- Find the latest release download URL ---

RELEASE_URL="https://github.com/$REPO/releases/latest/download/$BINARY"

# --- Pick install directory ---

if [ -w /usr/local/bin ]; then
    INSTALL_DIR="/usr/local/bin"
else
    INSTALL_DIR="$HOME/.local/bin"
    mkdir -p "$INSTALL_DIR"
fi

DEST="$INSTALL_DIR/vclaw"

echo "Downloading $BINARY from latest release..."

if command -v curl >/dev/null 2>&1; then
    curl -fSL "$RELEASE_URL" -o "$DEST"
elif command -v wget >/dev/null 2>&1; then
    wget -q "$RELEASE_URL" -O "$DEST"
else
    echo "Error: curl or wget is required to download vclaw."
    exit 1
fi

chmod +x "$DEST"

echo ""
echo "vclaw installed to $DEST"

# Check if install dir is on PATH
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        echo ""
        echo "Note: $INSTALL_DIR is not on your PATH."
        echo "Add it with:"
        echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
        ;;
esac

echo ""
echo "Run 'vclaw' to get started."
