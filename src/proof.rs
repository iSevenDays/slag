use crate::error::SlagError;
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
    run_shell_with_timeout(cmd, timeout_secs).await
}

pub async fn run_shell_with_timeout(cmd: &str, timeout_secs: u64) -> (bool, String) {
    let mut command = tokio::process::Command::new("bash");
    command
        .args(["-c", cmd])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
