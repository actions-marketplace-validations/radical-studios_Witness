#!/bin/sh
# Installs the witness binary from GitHub Releases, checking its SHA-256.
#
#   curl -fsSL https://raw.githubusercontent.com/radical-studios/witness/main/install.sh | sh
#   sh install.sh --version v0.2.0 --to /usr/local/bin
#
# Environment: WITNESS_VERSION (default: latest), WITNESS_INSTALL_DIR
# (default: ~/.local/bin), WITNESS_DOWNLOAD_URL (a mirror of the release
# files, instead of GitHub).
set -eu

REPO="radical-studios/witness"
VERSION="${WITNESS_VERSION:-latest}"
DEST="${WITNESS_INSTALL_DIR:-$HOME/.local/bin}"

while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --to) DEST="$2"; shift 2 ;;
    -h|--help) echo 'usage: install.sh [--version vX.Y.Z|latest] [--to DIR]'; exit 0 ;;
    *) echo "install.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

from_source="cargo install --git https://github.com/$REPO witness --locked"
case "$(uname -s)" in
  Linux) os="unknown-linux-musl" ;;
  Darwin) os="apple-darwin" ;;
  *) echo "install.sh: no prebuilt binary for $(uname -s); on Windows use install.ps1, or run: $from_source" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *) echo "install.sh: no prebuilt binary for $(uname -m); run: $from_source" >&2; exit 1 ;;
esac
asset="witness-$arch-$os.tar.gz"
if [ -n "${WITNESS_DOWNLOAD_URL:-}" ]; then
  base="$WITNESS_DOWNLOAD_URL"
elif [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fetch() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  else
    wget -qO "$2" "$1"
  fi
}
fetch "$base/$asset" "$tmp/$asset"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"

expected="$(grep " $asset\$" "$tmp/SHA256SUMS" | cut -d ' ' -f 1)"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)"
else
  actual="$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)"
fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "install.sh: checksum mismatch for $asset; not installing" >&2
  exit 1
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$DEST"
cp "$tmp/witness" "$DEST/witness"
chmod 755 "$DEST/witness"
echo "witness: installed $("$DEST/witness" --version) to $DEST/witness"
case ":$PATH:" in
  *":$DEST:"*) ;;
  *) echo "witness: add $DEST to your PATH" ;;
esac
