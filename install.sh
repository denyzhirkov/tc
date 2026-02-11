#!/bin/sh
set -e

REPO="denyzhirkov/tc"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "${OS}" in
  Darwin)
    case "${ARCH}" in
      arm64)  SUFFIX="macos-arm64" ;;
      x86_64) SUFFIX="macos-amd64" ;;
      *)      echo "Unsupported architecture: ${ARCH}"; exit 1 ;;
    esac
    ;;
  Linux)
    case "${ARCH}" in
      x86_64) SUFFIX="linux-amd64" ;;
      *)      echo "Unsupported architecture: ${ARCH}"; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: ${OS} (use install.ps1 for Windows)"
    exit 1
    ;;
esac

CLIENT_URL="https://github.com/${REPO}/releases/latest/download/tc-client-${SUFFIX}"
SERVER_URL="https://github.com/${REPO}/releases/latest/download/tc-server-${SUFFIX}"
INSTALL_DIR="/usr/local/bin"

echo "Downloading tc-client (${SUFFIX})..."
curl -fSL -o /tmp/tc-client "${CLIENT_URL}"

echo "Downloading tc-server (${SUFFIX})..."
curl -fSL -o /tmp/tc-server "${SERVER_URL}"

chmod +x /tmp/tc-client /tmp/tc-server

if [ -w "${INSTALL_DIR}" ]; then
  mv /tmp/tc-client "${INSTALL_DIR}/tc-client"
  mv /tmp/tc-server "${INSTALL_DIR}/tc-server"
  ln -sf "${INSTALL_DIR}/tc-client" "${INSTALL_DIR}/tc"
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo mv /tmp/tc-client "${INSTALL_DIR}/tc-client"
  sudo mv /tmp/tc-server "${INSTALL_DIR}/tc-server"
  sudo ln -sf "${INSTALL_DIR}/tc-client" "${INSTALL_DIR}/tc"
fi

echo "Installed tc-client and tc-server to ${INSTALL_DIR}"
echo "Run 'tc' to start the client."
