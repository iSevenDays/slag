# Slag - Local Learnings

## Retry Prompt Missing CMD: Instruction
**Problem:** Ingots on heat 2+ would fail with "smith output missing CMD: line" even though the underlying work was done correctly. This looked like a Claude protocol failure but was actually a prompt bug.
**Root cause:** `src/flux.rs` has two prompt paths - first attempt (`slag` is None) includes "End with exactly: CMD: <shell command to verify>", but the retry path (`slag` is Some) only sends the cracked warning without the CMD instruction. Claude follows instructions literally, so no instruction = no CMD output.
**Solution:** Add `flux.push_str("End with exactly: CMD: <shell command to verify>\n\n");` to the retry branch (after the CRACKED/ANALYZE block). Fixed in v1.3.3.

## Release Workflow: Version Bump Checklist
**Problem:** Version must be bumped in multiple places or the release gets inconsistent.
**Files to update:** `Cargo.toml` (version field), `website/index.html` (softwareVersion in JSON-LD). Cargo.lock updates automatically on build. Website must be rebuilt and deployed separately via wrangler.
**Sequence:** Edit Cargo.toml -> cargo build (updates Cargo.lock) -> edit website/index.html -> npm run build in website/ -> wrangler deploy -> git add all -> commit -> tag -> push.
