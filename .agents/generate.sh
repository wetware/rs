#!/usr/bin/env bash
#
# Generate .claude/skills/ from .agents/skills/ww-*.md
#
# Each .agents/skills/ww-*.md file has YAML frontmatter with:
#   name, description, reads (optional)
#
# This script creates .claude/skills/{name}/SKILL.md with
# Claude Code frontmatter format + the skill body.
#
# Usage: bash .agents/generate.sh
#        make agent-skills

set -euo pipefail

AGENTS_DIR=".agents/skills"
CLAUDE_DIR=".claude/skills"

# Default allowed tools for all generated skills
ALLOWED_TOOLS="  - Read
  - Glob
  - Grep
  - Bash
  - Agent
  - AskUserQuestion"

for src in "$AGENTS_DIR"/ww-*.md; do
  [ -f "$src" ] || continue

  # Parse frontmatter fields
  name=""
  description=""
  in_frontmatter=false
  frontmatter_end=0

  while IFS= read -r line; do
    if [ "$frontmatter_end" -eq 0 ] && [ "$line" = "---" ]; then
      if $in_frontmatter; then
        frontmatter_end=1
        break
      else
        in_frontmatter=true
        continue
      fi
    fi

    if $in_frontmatter; then
      case "$line" in
        name:*)     name="${line#name: }" ;;
        description:*)  description="${line#description: }" ;;
      esac
    fi
  done < "$src"

  if [ -z "$name" ]; then
    echo "SKIP $src: no name in frontmatter" >&2
    continue
  fi

  # Extract body (everything after second ---)
  body=$(awk 'BEGIN{n=0} /^---$/{n++; if(n==2){found=1; next}} found{print}' "$src")

  # Create output directory
  dest="$CLAUDE_DIR/$name"
  mkdir -p "$dest"

  # Write SKILL.md with Claude Code frontmatter
  cat > "$dest/SKILL.md" <<EOF
---
name: $name
description: $description
allowed-tools:
$ALLOWED_TOOLS
---
$body
EOF

  echo "OK $name -> $dest/SKILL.md"
done

echo "Done. $(ls -d "$CLAUDE_DIR"/ww-*/ 2>/dev/null | wc -l | tr -d ' ') skills generated."
