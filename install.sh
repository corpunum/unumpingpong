#!/bin/sh
# UnumPingPong installer (upp + uppd) for Linux and macOS.
#
#   curl -fsSL https://github.com/corpunum/unumpingpong/releases/latest/download/install.sh | sh
#
# Downloads the prebuilt binary for this OS/arch from GitHub Releases, verifies
# it against the release's SHA256SUMS and installs upp and uppd into $UNUMPINGPONG_INSTALL_DIR
# (default ~/.local/bin). Nothing else is changed: no service, no config.
#
# Environment:
#   UNUMPINGPONG_VERSION      tag to install (default: latest release), e.g. v0.1.0
#   UNUMPINGPONG_INSTALL_DIR  destination directory (default: $HOME/.local/bin)
#   UNUMPINGPONG_REPO         owner/repo (default: corpunum/unumpingpong)
#   UNUMPINGPONG_DOWNLOAD_BASE  mirror URL holding the release files (overrides REPO/VERSION URLs;
#                             UNUMPINGPONG_VERSION must then be set explicitly)
set -eu

REPO="${UNUMPINGPONG_REPO:-corpunum/unumpingpong}"
VERSION="${UNUMPINGPONG_VERSION:-latest}"
DEST="${UNUMPINGPONG_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'unumpingpong-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

fetch() { # url outfile
  if command -v curl >/dev/null 2>&1; then curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then wget -q -O "$2" "$1"
  else die "need curl or wget"; fi
}

os=$(uname -s); arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) die "unsupported architecture: $arch (build from source: cargo install --git https://github.com/$REPO)" ;;
esac
case "$os" in
  Linux) target="$arch-unknown-linux-musl" ;;
  Darwin) target="$arch-apple-darwin" ;;
  *) die "unsupported OS: $os (on Windows use install.ps1)" ;;
esac

if [ -n "${UNUMPINGPONG_DOWNLOAD_BASE:-}" ]; then
  [ "$VERSION" != latest ] || die "set UNUMPINGPONG_VERSION when using UNUMPINGPONG_DOWNLOAD_BASE"
elif [ "$VERSION" = latest ]; then
  # Resolve the tag through the redirect of /releases/latest (no API token needed).
  if command -v curl >/dev/null 2>&1; then
    VERSION=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" | sed 's#.*/tag/##')
  else
    VERSION=$(wget -S --spider "https://github.com/$REPO/releases/latest" 2>&1 | sed -n 's#.*Location: .*/tag/\([^ ]*\).*#\1#p' | tail -1)
  fi
  [ -n "$VERSION" ] || die "could not resolve the latest release"
fi

base="${UNUMPINGPONG_DOWNLOAD_BASE:-https://github.com/$REPO/releases/download/$VERSION}"
archive="unumpingpong-$VERSION-$target.tar.gz"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $archive"
fetch "$base/$archive" "$tmp/$archive" || die "download failed: $base/$archive"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"

expected=$(awk -v f="$archive" '$2 == f || $2 == "*"f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "$archive is not listed in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum "$tmp/$archive" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then actual=$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')
else die "need sha256sum or shasum to verify the download"; fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $archive (expected $expected, got $actual)"
say "checksum ok"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$DEST"
# Install via a temp file + rename so a running daemon keeps its old inode.
for bin in upp uppd; do
  cp "$tmp/unumpingpong-$VERSION-$target/$bin" "$DEST/.$bin.new"
  chmod 755 "$DEST/.$bin.new"
  mv -f "$DEST/.$bin.new" "$DEST/$bin"
done
say "installed $("$DEST/upp" --version) and uppd to $DEST"
case ":$PATH:" in *":$DEST:"*) ;; *) say "note: $DEST is not on PATH" ;; esac
