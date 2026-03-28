# Slag - Local Learnings

## Retry Prompt Missing CMD: Instruction
**Problem:** Ingots on heat 2+ would fail with "smith output missing CMD: line" even though the underlying work was done correctly. This looked like a Claude protocol failure but was actually a prompt bug.
**Root cause:** `src/flux.rs` has two prompt paths - first attempt (`slag` is None) includes "End with exactly: CMD: <shell command to verify>", but the retry path (`slag` is Some) only sends the cracked warning without the CMD instruction. Claude follows instructions literally, so no instruction = no CMD output.
**Solution:** Add `flux.push_str("End with exactly: CMD: <shell command to verify>\n\n");` to the retry branch (after the CRACKED/ANALYZE block). Fixed in v1.3.3.

## Release Workflow: Version Bump Checklist
**Problem:** Version must be bumped in multiple places or the release gets inconsistent.
**Files to update:** `Cargo.toml` (version field), `website/index.html` (softwareVersion in JSON-LD). Cargo.lock updates automatically on build. Website must be rebuilt and deployed separately via wrangler.
**Sequence:** Edit Cargo.toml -> cargo build (updates Cargo.lock) -> edit website/index.html -> npm run build in website/ -> wrangler deploy -> git add all -> commit -> tag -> push.

## Self-Improve: strip_artifacts Deletes Upstream Files
**Problem:** `strip_artifacts()` used `std::fs::remove_file()` + `git add -A` to remove slag forge artifacts (PLAN.md, BLUEPRINT.md, etc.) before merging the self-improve branch. But files like PROGRESS.md exist on upstream — deleting them stages a deletion that leaks into the PR diff.
**Root cause:** `remove_file` + `add -A` treats the deletion as a change. PR #1 had a 200-line PROGRESS.md deletion alongside the 5-line test addition.
**Solution:** Use `git checkout origin/main -- {artifact}` to restore upstream versions instead of deleting. Only `remove_file` for artifacts that didn't exist upstream (like logs/). Fixed in `src/self_improve.rs`.

## Self-Improve: gh repo fork --clone Syntax
**Problem:** Initial attempt used `gh repo fork GH_REPO --clone --remote --clone-dir=/tmp/path` which silently failed. The `--clone-dir` flag doesn't exist on `gh repo fork`.
**Solution:** Use `gh repo fork GH_REPO --clone -- /tmp/path` — the clone directory is passed after `--` as a positional arg to the underlying `git clone`. The `gh` CLI passes everything after `--` through to git.

## Self-Improve: Merge Fails on Dirty Working Tree
**Problem:** `slag self-improve` runs the forge pipeline which takes minutes. During that time, auto-commit hooks can modify PROGRESS.md in the main repo. When self-improve tries to `git merge self-improve`, it fails with "Your local changes to PROGRESS.md would be overwritten by merge."
**Solution:** `git stash --include-untracked` before merge, `git stash pop` after. Added to `merge_and_cleanup()` in `src/self_improve.rs`.

## Self-Improve: ANTHROPIC_API_KEY Blocks Claude Auto-Detection
**Problem:** When `ANTHROPIC_API_KEY` is set, slag's auto-detection skips Claude CLI to avoid accidental API billing. Running `slag self-improve` without `SLAG_SMITH=claude` may pick codex/gemini instead.
**Solution:** Always set `SLAG_SMITH=claude` when running self-improve with Claude Max subscription: `SLAG_SMITH=claude slag self-improve quality`.

## Infra Retries: Identical Failure Detection Must Skip Structured Headers
**Problem:** After adding `format_slag_message()` with structured headers (`=== HEAT FAILED ===`, `Type:`, `Exit:`), the `failure_signature()` function was matching these static headers as the "signature" of every failure, causing all failures to look identical and triggering the 3-identical-failures bail.
**Solution:** Updated `failure_signature()` to filter out lines starting with `=== HEAT`, `Type:`, `Exit:`, `CMD:`, `Files changed:`, `===` before extracting the signature. This way only the actual error content is compared.

## Adding New Field to Ingot Struct: Budget of Constructors
**Problem:** Adding `budget: Option<u64>` to the `Ingot` struct in `src/sexp/mod.rs` caused 13 compilation errors across 7 files — every test helper and constructor that creates an `Ingot` literal needs the new field.
**Solution:** Search all files for `Ingot {` and add `budget: None,` to each. Files: `sexp/writer.rs` (4), `crucible.rs` (3), `pipeline/founder.rs` (1), `pipeline/resmelt.rs` (1), `pipeline/forge.rs` (1), `pipeline/analysis.rs` (1), `pipeline/outcome.rs` (2). Use a subagent for bulk edits.
