#!/usr/bin/env bash
# Release notes for TAG: the feat, fix and perf commits since the previous v* tag, then how to check
# the download. Run in a checkout that has the tags.
#
# Usage: scripts/release-notes.sh TAG
set -euo pipefail

tag=${1:?usage: release-notes.sh TAG}
prev=$(git describe --tags --abbrev=0 --match 'v*' "$tag^" 2>/dev/null || true)

echo "Changes since ${prev:-the first commit}:"
echo
git log --no-merges --format=%s "${prev:+$prev..}$tag" |
  grep -E '^(feat|fix|perf)(\([^)]*\))?!?: ' | sed 's/^/- /' || true
echo
echo "Check a download against SHA256SUMS, or with \`gh attestation verify FILE --repo heidari-dev/kite\`."
echo "Every file also has a Sigstore bundle (FILE.sigstore.json). See docs/releasing.md."
