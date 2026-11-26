#!/usr/bin/env bash
# The version the release workflow builds, written to $GITHUB_OUTPUT as `version=`, and
# for a nightly `nightly=<build>`, or `skip=true` when there's nothing new to build one
# from. Run from the checkout, with every tag fetched.
#
#   NIGHTLY=true APP_PATHS="src ..." bash .github/scripts/nightly-version.sh
#
# A release builds Cargo.toml's version. A nightly is named after the version it leads
# up to, so that it comes before that release and after the last one:
# <version>-nightly.<date>, with .2, .3 for more on the same day. That version is
# Cargo.toml's while it's unreleased (no v<version> tag yet), so bumping it by hand
# (to 0.5.0, say) names the nightlies after it; once it's released, the next patch.
# NIGHTLY_DAY stands in for today's date (Central, like the schedule), for testing.
set -euo pipefail

out=${GITHUB_OUTPUT:-/dev/stdout}
cargo_ver=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
if [ "${NIGHTLY:-}" != true ]; then
  echo "version=$cargo_ver" >> "$out"
  exit 0
fi
if [ -n "$(git tag --points-at HEAD 'v*')" ]; then
  echo "HEAD already has a release or nightly: $(git tag --points-at HEAD 'v*')"
  echo "skip=true" >> "$out"
  exit 0
fi
# Nothing to build if nothing in the app changed (APP_PATHS).
read -ra app <<< "${APP_PATHS:?}"
prev=$(git describe --tags --abbrev=0 --match 'v*' HEAD 2>/dev/null || true)
if [ -n "$prev" ] && [ -z "$(git diff --name-only "$prev" HEAD -- "${app[@]}")" ]; then
  echo "Nothing in the app changed since $prev"
  echo "skip=true" >> "$out"
  exit 0
fi
if [[ ! "$cargo_ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Cargo.toml's version isn't major.minor.patch: $cargo_ver" >&2
  exit 1
fi
# Cargo's version, unless it's released already (or below the newest release, tagged on
# a branch): then the patch after the newest release. A nightly named after a released
# version would come before it, and never be offered. 2.0.x count as 0.3.4.x (see
# src/update.rs), so they aren't the newest.
latest=$(git tag -l 'v*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | grep -v '^v2\.0\.' | sed 's/^v//' | sort -V | tail -1 || true)
base=$cargo_ver
if [ -n "$latest" ] && [ "$(printf '%s\n' "$cargo_ver" "$latest" | sort -V | tail -1)" = "$latest" ]; then
  IFS=. read -r major minor patch <<< "$latest"
  base="$major.$minor.$((patch + 1))"
fi
day=${NIGHTLY_DAY:-$(TZ=America/Chicago date +%Y%m%d)}
build=$day
n=1
while git rev-parse -q --verify "refs/tags/v$base-nightly.$build" >/dev/null; do
  n=$((n + 1)); build="$day.$n"
done
echo "Nightly $base-nightly.$build"
echo "version=$base-nightly.$build" >> "$out"
echo "nightly=$build" >> "$out"
