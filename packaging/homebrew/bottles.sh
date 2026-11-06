#!/bin/sh
# Repackage the release archives as Homebrew bottles.
#
#   packaging/homebrew/bottles.sh <version> <dist dir> [program]
#
# For every <program>-<target>.tar.gz in <dist dir> (program: tracebridge,
# the default, or rvdbg), writes <program>-<version>.<tag>.bottle.tar.gz and
# its .sha256 next to it. A bottle is the installed keg as a tarball:
# <program>/<version>/bin/...
# Homebrew pours a bottle without building anything, so installing from the
# tap does not need Xcode or the Command Line Tools.
#
# Bottle tags: a bottle serves its macOS version and every later one. The
# binary runs on macOS 11 and later and links nothing from Homebrew, so the
# Apple silicon bottle is tagged Sonoma whatever runner built it. Intel Macs
# are not supported.
set -eu

version="${1:?version, e.g. 0.1.0}"
dist="${2:?directory with <program>-<target>.tar.gz}"
program="${3:-tracebridge}"

tag_of() {
    case "$1" in
        aarch64-apple-darwin) echo arm64_sonoma ;;
        aarch64-unknown-linux-musl) echo arm64_linux ;;
        x86_64-unknown-linux-musl) echo x86_64_linux ;;
        *) echo "bottles.sh: no bottle tag for $1" >&2; exit 1 ;;
    esac
}

checksum() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for archive in "$dist"/"$program"-*.tar.gz; do
    base="$(basename "$archive" .tar.gz)"
    case "$base" in *.bottle) continue ;; esac
    target="${base#"$program"-}"
    tag="$(tag_of "$target")"
    rm -rf "$work/unpack" "$work/$program"
    mkdir -p "$work/unpack" "$work/$program/$version/bin"
    tar -xzf "$archive" -C "$work/unpack"
    cp "$work/unpack/$base/$program" "$work/$program/$version/bin/"
    cp "$work/unpack/$base/LICENSE" "$work/unpack/$base/NOTICE" "$work/unpack/$base/README.md" \
        "$work/$program/$version/"
    chmod 755 "$work/$program/$version/bin/$program"
    bottle="$program-$version.$tag.bottle.tar.gz"
    tar -C "$work" -czf "$dist/$bottle" "$program"
    (cd "$dist" && checksum "$bottle" > "$bottle.sha256")
    echo "$bottle"
done
