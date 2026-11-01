#!/usr/bin/env bash
# Publishes ruos: the crate on crates.io and, on the GitHub release of the
# same version (vVERSION), the objects the crate leaves out (sqlited,
# sqlite3: ruos fetches them from there when first used).
#   ./publish.sh [-y] [--dry-run]
#     -y         do not ask before the steps that cannot be undone
#     --dry-run  check and build everything, publish nothing
# Needs: everything committed and pushed, `gh auth login`, `cargo login`.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")" && pwd)
cd "$ROOT"
yes=0 dry=0
for a in "$@"; do
    case $a in
        -y) yes=1 ;;
        --dry-run) dry=1 ;;
        *) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
    esac
done
die() { echo "publish.sh: $*" >&2; exit 1; }
step() { printf '\n== %s\n' "$*"; }

v=$(sed -n 's/^version = "\(.*\)"/\1/p' ruos/Cargo.toml)
tag="v$v"
repo=$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null) || die "gh cannot see the repository (gh auth login)"

step "checks: ruos $v, release $tag on $repo"
[ -z "$(git status --porcelain)" ] || { git status --short >&2; die "commit your changes first"; }
git fetch -q origin || die "git fetch failed"
branch=$(git rev-parse --abbrev-ref HEAD)
[ "$(git rev-parse HEAD)" = "$(git rev-parse "origin/$branch" 2>/dev/null)" ] || die "push $branch first (the release's tag is made on GitHub at this commit)"
code=$(curl -s -o /dev/null -w '%{http_code}' -A 'ruos publish.sh' "https://crates.io/api/v1/crates/ruos/$v")
[ "$code" = 404 ] || die "ruos $v is already on crates.io (HTTP $code): raise the version in ruos/Cargo.toml"
if [ $dry = 0 ]; then
    [ -n "${CARGO_REGISTRY_TOKEN:-}" ] || [ -f "${CARGO_HOME:-$HOME/.cargo}/credentials.toml" ] || die "no crates.io token (cargo login)"
fi
echo "ok"

step "build (run.sh dist)"
./run.sh dist

step "package (cargo publish --dry-run)"
(cd ruos && cargo publish --dry-run --allow-dirty --quiet)
crate=$(ls -t target/package/ruos-"$v".crate | head -1)
echo "$crate: $(du -h "$crate" | cut -f1) (crates.io takes up to 10 MiB)"

# What the release must have: every file of build/release.
need=$(ls build/release)
have=$(gh release view "$tag" --json assets -q '.assets[].name' 2>/dev/null || true)
missing=$(comm -23 <(printf '%s\n' "$need" | sort) <(printf '%s\n' "$have" | sed '/^$/d' | sort))

if [ $dry = 1 ]; then
    step "dry run: nothing published"
    if gh release view "$tag" > /dev/null 2>&1; then
        echo "release $tag: $(printf '%s' "$missing" | grep -c . || true) of $(printf '%s\n' "$need" | grep -c .) objects missing"
    else
        echo "release $tag: not there yet (it would be made with $(printf '%s\n' "$need" | grep -c .) objects)"
    fi
    exit 0
fi

if [ $yes = 0 ]; then
    printf '\nPublish ruos %s to crates.io and the objects to release %s? (a version cannot be published twice) [y/N] ' "$v" "$tag"
    read -r ok
    [ "$ok" = y ] || [ "$ok" = Y ] || die "stopped"
fi

step "release $tag"
if gh release view "$tag" > /dev/null 2>&1; then
    if [ -n "$missing" ]; then
        (cd build/release && gh release upload "$tag" $missing)
    fi
else
    gh release create "$tag" build/release/* --target "$(git rev-parse HEAD)" --title "ruos $v" \
        --notes "The objects of the images that the crate leaves out (sqlited, sqlite3): ruos fetches them from here when they are first used, and checks each against its BLAKE3 id."
fi
left=$(comm -23 <(printf '%s\n' "$need" | sort) <(gh release view "$tag" --json assets -q '.assets[].name' | sort))
[ -z "$left" ] || die "the release still lacks: $left"
echo "release $tag has all $(printf '%s\n' "$need" | grep -c .) objects"

step "crates.io"
(cd ruos && cargo publish --allow-dirty)

step "done: cargo install ruos (version $v)"
