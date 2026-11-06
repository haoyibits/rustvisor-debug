#!/bin/sh
# Install rvdbg from GitHub releases. The StellarLINK backend also needs the
# patched OpenOCD (Homebrew: haoyibits/tap/openocd-stellar), which this
# script does not install.
#
#   curl -fsSL https://github.com/haoyibits/rustvisor-debug/releases/latest/download/install.sh | sh
#
# Environment:
#   RVDBG_VERSION      release tag to install (default: latest), e.g. v0.1.0
#   RVDBG_INSTALL_DIR  target directory (default: ~/.local/bin)
#   RVDBG_REPO         GitHub repository (default: haoyibits/rustvisor-debug)
#   RVDBG_BASE_URL     download from this URL instead of GitHub (mirrors, testing)
set -eu

repo="${RVDBG_REPO:-haoyibits/rustvisor-debug}"
version="${RVDBG_VERSION:-latest}"
install_dir="${RVDBG_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    echo "install.sh: $*" >&2
    exit 1
}

case "$(uname -s)" in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-musl ;;
    *) fail "unsupported operating system: $(uname -s)" ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
esac
[ "$os-$arch" = apple-darwin-x86_64 ] && fail "Intel Macs are not supported; use an Apple silicon Mac"
target="$arch-$os"
asset="rvdbg-$target.tar.gz"
if [ -n "${RVDBG_BASE_URL:-}" ]; then
    base="$RVDBG_BASE_URL"
elif [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    base="https://github.com/$repo/releases/download/$version"
fi

command -v curl >/dev/null 2>&1 || fail "curl is required"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT INT TERM

echo "Downloading $asset ($version)"
curl -fsSL "$base/$asset" -o "$temporary/$asset" || fail "cannot download $base/$asset"
curl -fsSL "$base/$asset.sha256" -o "$temporary/$asset.sha256" || fail "cannot download the checksum"

expected="$(cut -d ' ' -f 1 "$temporary/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$temporary/$asset" | cut -d ' ' -f 1)"
else
    actual="$(shasum -a 256 "$temporary/$asset" | cut -d ' ' -f 1)"
fi
[ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"

tar -xzf "$temporary/$asset" -C "$temporary"
mkdir -p "$install_dir"
cp "$temporary/rvdbg-$target/rvdbg" "$install_dir/rvdbg.new"
chmod 755 "$install_dir/rvdbg.new"
if [ "$os" = apple-darwin ]; then
    xattr -d com.apple.quarantine "$install_dir/rvdbg.new" 2>/dev/null || true
fi
mv -f "$install_dir/rvdbg.new" "$install_dir/rvdbg"

echo "Installed $("$install_dir/rvdbg" --version) to $install_dir/rvdbg"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "Note: $install_dir is not on PATH; add it to your shell profile, e.g. export PATH=\"$install_dir:\$PATH\"" ;;
esac
