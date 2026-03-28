use crate::error::SlagError;
use std::path::Path;
use std::time::Duration;
use tokio::time::timeout;

/// Extract `CMD: <command>` from smith response text.
/// Takes the last CMD: line found (smith may output multiple).
pub fn extract_cmd(response: &str) -> Option<String> {
    response
        .lines()
        .rev()
        .find(|line| line.starts_with("CMD:"))
        .map(|line| line.strip_prefix("CMD:").unwrap().trim().to_string())
}

/// Run a shell command and return (success, output).
pub async fn run_shell(cmd: &str) -> (bool, String) {
    let timeout_secs = std::env::var("SLAG_PROOF_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(120);
    run_shell_with_timeout_in_dir(cmd, timeout_secs, None).await
}

pub async fn run_shell_with_timeout(cmd: &str, timeout_secs: u64) -> (bool, String) {
    run_shell_with_timeout_in_dir(cmd, timeout_secs, None).await
}

/// Run a shell command in a specific directory and return (success, output).
pub async fn run_shell_in_dir(cmd: &str, dir: &Path) -> (bool, String) {
    let timeout_secs = std::env::var("SLAG_PROOF_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(120);
    run_shell_with_timeout_in_dir(cmd, timeout_secs, Some(dir)).await
}

async fn run_shell_with_timeout_in_dir(
    cmd: &str,
    timeout_secs: u64,
    dir: Option<&Path>,
) -> (bool, String) {
    if let Some(reason) = blocked_shell_reason(cmd) {
        return (
            false,
            format!("blocked dangerous command in proof/test: {reason}"),
        );
    }

    let mut command = tokio::process::Command::new("bash");
    command
        .args(["-c", cmd])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return (false, format!("spawn error: {e}")),
    };

    match timeout(Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            (output.status.success(), combined)
        }
        Ok(Err(e)) => (false, format!("wait error: {e}")),
        Err(_) => (
            false,
            format!("timeout after {timeout_secs}s: command did not finish"),
        ),
    }
}

fn blocked_shell_reason(cmd: &str) -> Option<&'static str> {
    let lowered = cmd.to_ascii_lowercase();

    if lowered.contains("rm -rf") {
        return Some("rm -rf");
    }
    if lowered.contains("git reset --hard") {
        return Some("git reset --hard");
    }
    if lowered.contains("git checkout --") {
        return Some("git checkout --");
    }
    if lowered.contains("git clean -fd") || lowered.contains("git clean -xdf") {
        return Some("git clean");
    }
    if lowered.contains("mkfs.") || lowered.contains("mkfs ") {
        return Some("mkfs");
    }
    if lowered.contains("dd if=/dev/zero of=/dev/") {
        return Some("dd to /dev");
    }
    if lowered.contains(":(){") || lowered.contains("fork bomb") {
        return Some("fork bomb");
    }

    None
}

/// Verify an ingot's proof command.
/// Returns Ok(()) if proof passes, Err with reason if it fails.
pub async fn verify_proof(proof: &str, id: &str) -> Result<(), SlagError> {
    if proof.is_empty() || proof == "true" {
        return Ok(());
    }

    let (success, output) = run_shell(proof).await;
    if success {
        Ok(())
    } else {
        Err(SlagError::ProofFailed {
            id: id.to_string(),
            reason: output,
        })
    }
}

/// Git add + commit with forge message
pub async fn git_commit(id: &str, work: &str) {
    let msg = format!("forge({id}): {work}");
    let _ = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .output()
        .await;
    let _ = tokio::process::Command::new("git")
        .args(["commit", "-m", &msg, "--quiet"])
        .output()
        .await;
}

/// Git experiment commit: stage all changes and commit with experiment prefix.
/// Returns the short commit hash on success.
pub async fn git_experiment_commit(id: &str, heat: u8) -> Option<String> {
    let msg = format!("experiment: {id} heat {heat}");
    let _ = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .output()
        .await;
    let output = tokio::process::Command::new("git")
        .args(["commit", "-m", &msg, "--quiet", "--allow-empty"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // Return short hash of the new commit
    tokio::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Git experiment commit in a specific directory (worktree).
pub async fn git_experiment_commit_in_dir(id: &str, heat: u8, dir: &str) -> Option<String> {
    let msg = format!("experiment: {id} heat {heat}");
    let _ = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .await;
    let output = tokio::process::Command::new("git")
        .args(["commit", "-m", &msg, "--quiet", "--allow-empty"])
        .current_dir(dir)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    tokio::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(dir)
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Revert the last commit (preserves it in history, unlike reset).
/// Uses `git revert HEAD --no-edit` so the failed experiment stays in `git log`.
pub async fn git_revert_last() -> bool {
    tokio::process::Command::new("git")
        .args(["revert", "HEAD", "--no-edit", "--no-commit"])
        .output()
        .await
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Revert the last commit in a specific directory (worktree).
pub async fn git_revert_last_in_dir(dir: &str) -> bool {
    tokio::process::Command::new("git")
        .args(["revert", "HEAD", "--no-edit", "--no-commit"])
        .current_dir(dir)
        .output()
        .await
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if git experiments mode is enabled (opt-in for non-worktree).
pub fn git_experiments_enabled() -> bool {
    std::env::var("SLAG_GIT_EXPERIMENTS")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Check if the working tree is clean (no uncommitted changes).
pub async fn git_is_clean() -> bool {
    tokio::process::Command::new("git")
        .args(["diff", "--quiet", "HEAD"])
        .output()
        .await
        .ok()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn extract_cmd_basic() {
        let response = "Created files...\nCMD: npm test\n";
        assert_eq!(extract_cmd(response), Some("npm test".to_string()));
    }

    #[test]
    fn extract_cmd_last() {
        let response = "CMD: echo first\nmore stuff\nCMD: echo second\n";
        assert_eq!(extract_cmd(response), Some("echo second".to_string()));
    }

    #[test]
    fn extract_cmd_none() {
        let response = "No command here\njust text\n";
        assert_eq!(extract_cmd(response), None);
    }

    #[test]
    fn extract_cmd_with_spaces() {
        let response = "CMD:   test -f package.json && npm test  \n";
        assert_eq!(
            extract_cmd(response),
            Some("test -f package.json && npm test".to_string())
        );
    }

    #[tokio::test]
    async fn run_shell_success() {
        let (ok, _) = run_shell("true").await;
        assert!(ok);
    }

    #[tokio::test]
    async fn run_shell_failure() {
        let (ok, _) = run_shell("false").await;
        assert!(!ok);
    }

    #[tokio::test]
    async fn run_shell_timeout() {
        let (ok, output) = run_shell_with_timeout("sleep 2", 1).await;
        assert!(!ok);
        assert!(output.contains("timeout after 1s"));
    }

    #[tokio::test]
    async fn run_shell_blocks_dangerous_command() {
        let (ok, output) = run_shell_with_timeout("rm -rf /tmp/anything", 10).await;
        assert!(!ok);
        assert!(output.contains("blocked dangerous command"));
    }

    #[tokio::test]
    async fn verify_proof_true() {
        assert!(verify_proof("true", "i1").await.is_ok());
    }

    #[tokio::test]
    async fn verify_proof_empty() {
        assert!(verify_proof("", "i1").await.is_ok());
    }

    #[tokio::test]
    async fn verify_proof_fails() {
        let result = verify_proof("test -f /nonexistent_file_xyz", "i1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn run_shell_in_dir_success() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ok.txt"), "ok").expect("write");
        let (ok, _) = run_shell_in_dir("test -f ok.txt", dir.path()).await;
        assert!(ok);
    }
}
