#!/bin/sh
set -e

REPO="denyzhirkov/tc"
INSTALL_SERVER=false

for arg in "$@"; do
  case "${arg}" in
    --server|--all) INSTALL_SERVER=true ;;
    --help|-h)
      echo "Usage: install.sh [--server]"
      echo "  --server  Also install tc-server"
      exit 0
      ;;
    *) echo "Unknown option: ${arg}"; exit 1 ;;
  esac
done

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
      x86_64)  SUFFIX="linux-amd64" ;;
      aarch64) SUFFIX="linux-arm64" ;;
      *)       echo "Unsupported architecture: ${ARCH}"; exit 1 ;;
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
chmod +x /tmp/tc-client

if [ "${INSTALL_SERVER}" = true ]; then
  echo "Downloading tc-server (${SUFFIX})..."
  curl -fSL -o /tmp/tc-server "${SERVER_URL}"
  chmod +x /tmp/tc-server
fi

if [ -w "${INSTALL_DIR}" ]; then
  mv /tmp/tc-client "${INSTALL_DIR}/tc-client"
  ln -sf "${INSTALL_DIR}/tc-client" "${INSTALL_DIR}/tc"
  if [ "${INSTALL_SERVER}" = true ]; then
    mv /tmp/tc-server "${INSTALL_DIR}/tc-server"
  fi
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo mv /tmp/tc-client "${INSTALL_DIR}/tc-client"
  sudo ln -sf "${INSTALL_DIR}/tc-client" "${INSTALL_DIR}/tc"
  if [ "${INSTALL_SERVER}" = true ]; then
    sudo mv /tmp/tc-server "${INSTALL_DIR}/tc-server"
  fi
fi

if [ "${INSTALL_SERVER}" = true ]; then
  echo "Installed tc-client and tc-server to ${INSTALL_DIR}"
else
  echo "Installed tc-client to ${INSTALL_DIR}"
fi
echo "Run 'tc' to start the client."
