#!/usr/bin/env bash
# Sync a repo's GitHub settings, labels and rulesets to the standard.
#
#   repo-sync.sh [owner/name]        default: origin remote of the current repo
#
#   EXTRA_CHECKS="Check A,Check B"   extra required status checks
#   CL4_APP_ID / TRASHBOT_APP_ID     exported values become the Actions variables
#
# Secret values are yours alone; this only reports which names are missing.
set -euo pipefail

REPO="${1:-${REPO:-$(git remote get-url origin | sed -E 's#(git@github\.com:|https://github\.com/)##; s#\.git$##')}}"
echo "syncing $REPO"

gh api -X PATCH "repos/$REPO" --input - >/dev/null <<'JSON'
{
  "allow_squash_merge": true,
  "allow_merge_commit": false,
  "allow_rebase_merge": false,
  "allow_auto_merge": true,
  "allow_update_branch": true,
  "delete_branch_on_merge": true,
  "has_issues": true,
  "has_wiki": false,
  "has_projects": false,
  "has_discussions": false,
  "has_downloads": false,
  "use_squash_pr_title_as_default": false,
  "web_commit_signoff_required": false,
  "merge_commit_title": "MERGE_MESSAGE",
  "merge_commit_message": "PR_TITLE",
  "squash_merge_commit_title": "COMMIT_OR_PR_TITLE",
  "squash_merge_commit_message": "COMMIT_MESSAGES",
  "security_and_analysis": {
    "secret_scanning": { "status": "enabled" },
    "secret_scanning_push_protection": { "status": "enabled" }
  }
}
JSON
echo "  settings"

while IFS='|' read -r name color desc; do
  [ -z "$name" ] && continue
  gh label create "$name" -R "$REPO" --color "$color" --description "$desc" >/dev/null 2>&1 || true
done <<'LABELS'
approve/cl4|0f16e8|Approve with cl4 bot
approve/trashbot-9000|f9a323|Approve with trashbot
area/ci|0075ca|
area/docker|ededed|
area/nix|7057ff|
area/python|ededed|
area/rust|ededed|
renovate/container|d4c5f9|
renovate/github-action|bfd4f2|
renovate/github-release|c2e0c6|
renovate/mise|f9d0c4|
status/release-pending|ededed|
status/released|ededed|
type/digest|bfdadc|
type/major|d93f0b|
type/minor|1d76db|
type/patch|0e8a16|
LABELS
echo "  labels"

upsert_ruleset() {
  local name="$1" rules="$2" id body
  id=$(gh api "repos/$REPO/rulesets" --jq ".[] | select(.name == \"$name\") | .id" | head -n1)
  body=$(jq -c -n --arg name "$name" --argjson rules "$rules" \
    '{name: $name, target: "branch", enforcement: "active",
      conditions: {ref_name: {include: ["~DEFAULT_BRANCH"], exclude: []}},
      rules: $rules, bypass_actors: []}')
  if [ -n "$id" ]; then
    gh api -X PUT "repos/$REPO/rulesets/$id" --input - <<<"$body" >/dev/null
  else
    gh api -X POST "repos/$REPO/rulesets" --input - <<<"$body" >/dev/null
  fi
  echo "  ruleset: $name"
}

CHECKS='["Code Quality", "Validate PR Conventions"]'
if [ -n "${EXTRA_CHECKS:-}" ]; then
  CHECKS=$(jq -c --arg extra "$EXTRA_CHECKS" \
    '. + ($extra | split(",") | map(gsub("^\\s+|\\s+$"; "")))' <<<"$CHECKS")
fi

upsert_ruleset "Branch Protection" \
  '[{"type":"deletion"},{"type":"non_fast_forward"},{"type":"required_linear_history"}]'

upsert_ruleset "CI Status Checks" \
  "$(jq -c -n --argjson checks "$CHECKS" \
    '[{type:"required_status_checks", parameters:{strict_required_status_checks_policy:false, do_not_enforce_on_create:true, required_status_checks: ($checks | map({context: .}))}}]')"

upsert_ruleset "PR Requirements" \
  '[{"type":"pull_request","parameters":{"required_approving_review_count":1,"dismiss_stale_reviews_on_push":true,"required_reviewers":[],"require_code_owner_review":true,"require_last_push_approval":true,"required_review_thread_resolution":true,"require_extra_approval_for_unattributed_changes":false,"allowed_merge_methods":["squash"]}}]'

upsert_ruleset "Testing" \
  '[{"type":"required_status_checks","parameters":{"strict_required_status_checks_policy":false,"do_not_enforce_on_create":true,"required_status_checks":[{"context":"Run Tests"}]}}]'

for s in CL4_APP_PRIVATE_KEY TRASHBOT_APP_PRIVATE_KEY; do
  gh api "repos/$REPO/actions/secrets/$s" >/dev/null 2>&1 \
    || echo "  MISSING secret $s - set it with: gh secret set $s -R $REPO"
done

for v in CL4_APP_ID TRASHBOT_APP_ID; do
  if [ -n "${!v:-}" ]; then
    gh variable set "$v" -R "$REPO" --body "${!v}" >/dev/null
    echo "  variable $v: set"
  elif ! gh api "repos/$REPO/actions/variables/$v" >/dev/null 2>&1; then
    echo "  MISSING variable $v - export $v and rerun"
  fi
done

echo "done"
