# Shared AI-attribution detection for git hooks.
# Sourced by commit-msg / pre-push. Not executable alone.
# shellcheck shell=bash

# Case-insensitive patterns that indicate AI-agent branding.
# Keep this list narrow enough to avoid false positives on legitimate
# domain words; widen only when a new tool starts injecting trailers.
AI_ATTR_PATTERNS=(
  # Common auto-inserted trailers / signatures
  'Generated with \[?[Dd]evin'
  'Generated with \[?[Cc]laude'
  'Generated with \[?[Cc]ursor'
  'Generated with \[?[Cc]opilot'
  'Generated with \[?[Cc]odex'
  'Generated [Bb]y (Claude|Copilot|Cursor|Devin|Codex|GPT|Gemini|ChatGPT)'
  'Co-[Aa]uthored-[Bb]y:.*([Dd]evin|[Cc]laude|[Aa]nthropic|[Cc]ursor|[Cc]opilot|[Oo]pen[Aa]I|[Gg]emini|[Cc]odex|[Ww]inds[uo]rf|[Cc]odeium|[Tt]abnine)'
  'Assisted-[Bb]y:.*([Dd]evin|[Cc]laude|[Aa]nthropic|[Cc]ursor|[Cc]opilot|[Oo]pen[Aa]I|[Gg]emini)'
  'Generated-[Bb]y:.*([Dd]evin|[Cc]laude|[Aa]nthropic|[Cc]ursor|[Cc]opilot|[Oo]pen[Aa]I|[Gg]emini)'
  # Bot / noreply author lines that should never appear in messages
  'noreply@anthropic\.com'
  'noreply@openai\.com'
  'devin-ai-integration\[bot\]'
  'copilot-swe-agent\[bot\]'
  # Emoji + boilerplate
  '🤖'
  'AI[- ]generated'
  'AI[- ]assisted'
  'This commit was (AI|auto)[- ]generated'
)

# Return 0 if $1 (file path or stdin via process substitution) matches any pattern.
ai_attribution_match() {
  local target=$1
  local pat
  for pat in "${AI_ATTR_PATTERNS[@]}"; do
    if grep -qiE -- "$pat" "$target" 2>/dev/null; then
      grep -iE -- "$pat" "$target" 2>/dev/null | head -3
      return 0
    fi
  done
  return 1
}

# Print a standardized rejection to stderr.
ai_attribution_reject() {
  local context=$1
  local match=$2
  cat >&2 <<EOF
error: AI-agent attribution detected in ${context}:

${match}

Policy: never add AI branding, "Generated with …", or Co-Authored-By
trailers for Devin/Claude/Cursor/Copilot/Codex/etc. to commits, PRs,
changelogs, or work-logs. See CONTRIBUTING.md.

Rewrite the message without the attribution and try again.
EOF
}
