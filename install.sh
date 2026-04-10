#!/usr/bin/env bash
set -euo pipefail

REPO="yinnho/aginx"

# 检测平台
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
    linux-x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
    linux-aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
    darwin-x86_64) TARGET="x86_64-apple-darwin" ;;
    darwin-arm64)  TARGET="aarch64-apple-darwin" ;;
    *)
        echo "Error: Unsupported platform ($OS $ARCH)"
        echo "Please build from source: cargo install --git https://github.com/$REPO"
        exit 1
        ;;
esac

# 获取最新版本
echo "Checking latest release..."
LATEST="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"v\(.*\)".*/\1/')"

if [ -z "$LATEST" ]; then
    echo "Error: Could not determine latest version"
    echo "Please build from source: cargo install --git https://github.com/$REPO"
    exit 1
fi

# 选择安装目录
if [ -w "/usr/local/bin" ]; then
    INSTALL_DIR="/usr/local/bin"
else
    INSTALL_DIR="${HOME}/.local/bin"
    mkdir -p "$INSTALL_DIR"
fi

BINARY="${INSTALL_DIR}/aginx"

# 下载
URL="https://github.com/$REPO/releases/download/v${LATEST}/aginx-${TARGET}"
echo "Downloading aginx v${LATEST} for ${TARGET}..."
curl -fsSL --progress-bar "$URL" -o "$BINARY"
chmod +x "$BINARY"

echo ""
echo "Installed aginx v${LATEST} to ${BINARY}"
echo ""

# 检查 PATH
case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        echo "Run: aginx"
        ;;
    *)
        echo "Add to PATH:"
        echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
        echo ""
        echo "Or add to ~/.bashrc / ~/.zshrc:"
        echo "  echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ~/.bashrc"
        ;;
esac
