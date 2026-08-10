#!/bin/sh
set -e

REPO="denyzhirkov/tc"

for arg in "$@"; do
  case "${arg}" in
    --help|-h)
      echo "Usage: install.sh"
      echo "  Installs tc-server to /usr/local/bin"
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
    echo "Unsupported OS: ${OS}"
    exit 1
    ;;
esac

SERVER_URL="https://github.com/${REPO}/releases/latest/download/tc-server-${SUFFIX}"
INSTALL_DIR="/usr/local/bin"

echo "Downloading tc-server (${SUFFIX})..."
curl -fSL -o /tmp/tc-server "${SERVER_URL}"
chmod +x /tmp/tc-server

if [ -w "${INSTALL_DIR}" ]; then
  mv /tmp/tc-server "${INSTALL_DIR}/tc-server"
else
  echo "Installing to ${INSTALL_DIR} (requires sudo)..."
  sudo mv /tmp/tc-server "${INSTALL_DIR}/tc-server"
fi

echo "Installed tc-server to ${INSTALL_DIR}"
echo "Run 'tc-server' to start the relay."
