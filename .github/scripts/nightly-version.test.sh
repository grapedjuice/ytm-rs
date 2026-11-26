#!/usr/bin/env bash
# Tests nightly-version.sh beside it in a throwaway repo, with the release workflow's
# APP_PATHS. Run by the Tests workflow.
#   bash .github/scripts/nightly-version.test.sh
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
script=$here/nightly-version.sh
APP_PATHS=$(grep -m1 '^  APP_PATHS:' "$here/../workflows/release.yml" | cut -d'"' -f2)
[ -n "$APP_PATHS" ] || { echo "no APP_PATHS in release.yml"; exit 1; }
export APP_PATHS
fail=0
repo=$(mktemp -d)
cd "$repo"
git init -q -b main
git config user.email t@t; git config user.name t
setver() { printf '[package]\nname = "ytm-rs"\nversion = "%s"\n' "$1" > Cargo.toml; }
change() { mkdir -p src; echo "$RANDOM$RANDOM" >> src/main.rs; git add -A; git commit -qm "${1:-change}"; }
run() { # NIGHTLY, day -> output lines
  NIGHTLY=$1 NIGHTLY_DAY=$2 GITHUB_OUTPUT=$repo/.out bash "$script" >/dev/null 2>"$repo/.err"; echo "rc=$?" >> "$repo/.out"
  cat "$repo/.out"; rm -f "$repo/.out"
}
expect() { # name, got, want
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  got:  $(echo "$2" | tr '\n' ' ')"; echo "  want: $(echo "$3" | tr '\n' ' ')"; cat "$repo/.err"; fail=1; fi
}

# 2.0.x, older than 0.4.0 to the updater, isn't the newest release.
setver 2.0.3; change "2.0.3"; git tag v2.0.3
setver 0.4.0; change "0.4.0"; git tag v0.4.0
# A release build: Cargo's version.
expect "release build" "$(run false 20261126)" "version=0.4.0
rc=0"
# HEAD is the release: skip.
expect "HEAD tagged" "$(run true 20261126)" "skip=true
rc=0"
# Only docs changed since: skip.
echo hi > README.md; git add -A; git commit -qm docs
expect "nothing in the app changed" "$(run true 20261126)" "skip=true
rc=0"
# An app change after 0.4.0 (tagged): the next patch.
change
expect "0.4.0 tagged -> 0.4.1-nightly" "$(run true 20261126)" "version=0.4.1-nightly.20261126
nightly=20261126
rc=0"
git tag v0.4.1-nightly.20261126
# The same commit again: already tagged.
expect "nightly HEAD tagged" "$(run true 20261126)" "skip=true
rc=0"
# Another change the same day: .2, then .3.
change
expect "same day .2" "$(run true 20261126)" "version=0.4.1-nightly.20261126.2
nightly=20261126.2
rc=0"
git tag v0.4.1-nightly.20261126.2
change
expect "same day .3" "$(run true 20261126)" "version=0.4.1-nightly.20261126.3
nightly=20261126.3
rc=0"
git tag v0.4.1-nightly.20261126.3
# describe finds the newest nightly tag; only docs since: skip.
echo more >> README.md; git add -A; git commit -qm docs2
expect "describe sees the new nightly tag" "$(git describe --tags --abbrev=0 --match 'v*' HEAD)" "v0.4.1-nightly.20261126.3"
expect "docs after a nightly: skip" "$(run true 20261127)" "skip=true
rc=0"
# Next day.
change
expect "next day" "$(run true 20261127)" "version=0.4.1-nightly.20261127
nightly=20261127
rc=0"
git tag v0.4.1-nightly.20261127
# An old-named nightly the same day doesn't count toward the new name's .2.
change
git tag v0.4.0-nightly.20261128
change
expect "old-named tag doesn't make a .2" "$(run true 20261128)" "version=0.4.1-nightly.20261128
nightly=20261128
rc=0"
# Bumped by hand to 0.5.0 (untagged): its own nightlies.
setver 0.5.0; change "0.5.0"
expect "0.5.0 untagged -> 0.5.0-nightly" "$(run true 20261129)" "version=0.5.0-nightly.20261129
nightly=20261129
rc=0"
expect "release build of 0.5.0" "$(run false 20261129)" "version=0.5.0
rc=0"
# Released: the next patch.
git tag v0.5.0; change
expect "0.5.0 tagged -> 0.5.1-nightly" "$(run true 20261130)" "version=0.5.1-nightly.20261130
nightly=20261130
rc=0"
# Cargo not bumped on main but 0.5.1 tagged elsewhere (a branch): past it to 0.5.2.
git tag v0.5.1 HEAD~1
change
expect "released patch skipped" "$(run true 20261130)" "version=0.5.2-nightly.20261130
nightly=20261130
rc=0"
# Cargo bumped to 0.5.1 by hand while 0.5.1 is tagged: still past it.
setver 0.5.1; change "0.5.1"
expect "Cargo at a released version" "$(run true 20261201)" "version=0.5.2-nightly.20261201
nightly=20261201
rc=0"
# 0.5.4 tagged on a branch, 0.5.3 never released: past the newest, not just the next.
git tag v0.5.4 HEAD~1
change
expect "a gap in released patches" "$(run true 20261201)" "version=0.5.5-nightly.20261201
nightly=20261201
rc=0"
# Bumped ahead of every release: Cargo's version again.
setver 0.10.0; change "0.10.0"
expect "0.10.0 sorts after 0.5.4" "$(run true 20261201)" "version=0.10.0-nightly.20261201
nightly=20261201
rc=0"
# A version that isn't major.minor.patch stops the nightly.
setver 0.6.0-beta.1; change
expect "bad version fails" "$(run true 20261202 | tail -1)" "rc=1"
# The real date, when NIGHTLY_DAY isn't given.
setver 0.6.0; change
day=$(TZ=America/Chicago date +%Y%m%d)
expect "today's date" "$(NIGHTLY=true GITHUB_OUTPUT=/dev/stdout bash "$script" | grep ^version)" "version=0.6.0-nightly.$day"

rm -rf "$repo"
[ $fail = 0 ] && echo "all ok"
exit $fail
