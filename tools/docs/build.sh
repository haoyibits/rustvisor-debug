#!/bin/sh
# Build both guide books and the language page into target/guide, then check
# every rendered local link. Run from anywhere; needs mdbook 0.5.4 and a
# python3 with tools/docs/requirements.txt.
set -eu
cd "$(dirname "$0")/../.."
rm -rf target/guide
mdbook build docs/guide/cn
mdbook build docs/guide/en
cp docs/guide/site/index.html docs/guide/site/404.html target/guide/
python3 tools/docs/links.py --check-html target/guide
