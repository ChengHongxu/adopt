#!/bin/sh
# End-to-end check against a real apt or dnf. It installs and removes packages,
# so only run it as root inside a throwaway container (CI does):
#
#   docker run --rm -v "$PWD:/src:ro" fedora:latest \
#     sh /src/tests/e2e/run.sh /src/target/x86_64-unknown-linux-musl/release/adopt
set -eu
BIN=${1:?usage: run.sh /path/to/adopt}

if command -v apt-get >/dev/null 2>&1; then PM=apt; else PM=dnf; fi
pm_install() {
    if [ $PM = apt ]; then DEBIAN_FRONTEND=noninteractive apt-get install -y -q "$@" >/dev/null
    else dnf -y -q install "$@" >/dev/null; fi
}
pm_remove() {
    if [ $PM = apt ]; then DEBIAN_FRONTEND=noninteractive apt-get remove -y -q "$@" >/dev/null
    else dnf -y -q remove "$@" >/dev/null; fi
}
installed() {
    if [ $PM = apt ]; then dpkg-query -W -f '${Status}' "$1" 2>/dev/null | grep -q 'install ok installed'
    else rpm -q "$1" >/dev/null 2>&1; fi
}
pass() { echo "ok   $*"; }
fail() { echo "FAIL $*" >&2; exit 1; }
prompt() { "$BIN" status --prompt; }
expect_prompt() {
    got=$(prompt)
    [ "$got" = "$1" ] && pass "status --prompt = '$1' ($2)" || fail "status --prompt: got '$got', want '$1' ($2)"
}

echo "== preparing $PM"
if [ $PM = apt ]; then apt-get update -q >/dev/null; fi
pm_install git
pm_remove jq bc tree 2>/dev/null || true
if [ $PM = dnf ]; then dnf --version | head -1; fi
"$BIN" --version

work=$(mktemp -d)
export HOME="$work/home"
export GIT_AUTHOR_NAME=e2e GIT_AUTHOR_EMAIL=e2e@example.invalid
export GIT_COMMITTER_NAME=e2e GIT_COMMITTER_EMAIL=e2e@example.invalid
mkdir -p "$HOME"
git config --global init.defaultBranch main
git init -q --bare -b main "$work/remote.git"
git clone -q "$work/remote.git" "$work/dots" 2>/dev/null

echo "== init"
"$BIN" init --yes --repo "$work/dots" --groups e2e
expect_prompt "" "baseline covers everything already installed"

echo "== a native install shows up"
pm_install tree
expect_prompt 1 "tree is new"

git clone -q "$work/remote.git" "$work/laptop"
publish() {
    printf '%s' "$1" > "$work/laptop/packages.toml"
    git -C "$work/laptop" commit -q -am "$2"
    git -C "$work/laptop" push -q
}

echo "== rule 1: new shared entries are installed"
publish 'version = 1
[system.jq]
[system.bc]
' "add jq bc"
"$BIN" sync --yes
installed jq && installed bc && pass "jq and bc installed" || fail "jq/bc not installed"

echo "== rule 2: a package the user removed is not reinstalled"
pm_remove jq
"$BIN" sync --yes
installed jq && fail "jq was reinstalled" || pass "jq stays removed"
expect_prompt 2 "tree is new, jq is missing"

echo "== rule 3: dropped entries are removed only if adopt installed them"
publish 'version = 1
[system.jq]
' "drop bc"
"$BIN" sync --yes
installed bc && fail "bc was not removed" || pass "bc removed"
installed tree && pass "tree (installed by hand, never claimed) untouched" || fail "tree was removed"

"$BIN" list
echo "== all good ($PM)"
