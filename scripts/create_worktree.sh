#!/bin/bash
set -e

# Get worktree name from parameter
WORKTREE_NAME=$1
if [ -z "$WORKTREE_NAME" ]; then
    echo "Usage: $0 <branch-name>"
    exit 1
fi

# Strip any prefix before slash for directory name
WORKTREE_DIR_NAME="${WORKTREE_NAME##*/}"

REPO_BASE_NAME="supabased"
WORKTREES_BASE="$HOME/wt/${REPO_BASE_NAME}"
WORKTREE_PATH="${WORKTREES_BASE}/${WORKTREE_DIR_NAME}"

echo "Creating worktree: ${WORKTREE_NAME}"
echo "Location: ${WORKTREE_PATH}"

# Create worktrees base if needed
mkdir -p "$WORKTREES_BASE"

# Check if worktree already exists
if [ -d "$WORKTREE_PATH" ]; then
    echo "Error: Worktree directory already exists: $WORKTREE_PATH"
    exit 1
fi

# Create worktree
git worktree add -b "$WORKTREE_NAME" "$WORKTREE_PATH"

# Copy .claude settings if they exist
if [ -f ".claude/settings.local.json" ]; then
    mkdir -p "$WORKTREE_PATH/.claude"
    cp .claude/settings.local.json "$WORKTREE_PATH/.claude/"
fi

# Copy .env files
for f in .env*; do [ -f "$f" ] && cp "$f" "$WORKTREE_PATH/"; done

echo "Worktree created successfully!"
echo "Path: ${WORKTREE_PATH}"
echo "Branch: ${WORKTREE_NAME}"
