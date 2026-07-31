#!/usr/bin/env bash
# print-release-asset-downloads.sh — breakdown of GitHub release asset downloads
set -euo pipefail

REPO="${1:-webstonehq/tuxedo}"

echo "Download breakdown for $REPO"
echo "============================================"

gh api --paginate "repos/$REPO/releases" --jq '
  .[] | "\(.tag_name)\t\([.assets[] | select(.name | endswith(".sha256") | not) | .download_count] | add // 0)\t\(.published_at)"
' | while IFS=$'\t' read -r tag total published; do
  printf "%-15s %8s downloads  (%s)\n" "$tag" "$total" "${published%T*}"
done

echo "--------------------------------------------"
TOTAL=$(gh api --paginate "repos/$REPO/releases" --jq '
  [.[].assets[] | select(.name | endswith(".sha256") | not) | .download_count] | add // 0
')
echo "TOTAL across all releases: $TOTAL"

if [ "$#" -gt 0 ]; then
  echo
  echo "Per-asset detail:"
  echo "--------------------------------------------"
  gh api --paginate "repos/$REPO/releases" --jq '
    .[] | .tag_name as $tag | .assets[] |
    "\($tag)\t\(.name)\t\(.download_count)"
  ' | while IFS=$'\t' read -r tag name count; do
    printf "%-12s %-40s %6s\n" "$tag" "$name" "$count"
  done
fi
