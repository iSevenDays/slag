use std::io::IsTerminal;
use std::time::Duration;

pub fn stdin_is_interactive() -> bool {
    std::io::stdin().is_terminal()
}

pub fn read_line_timeout(timeout_secs: u64) -> Option<String> {
    if timeout_secs == 0 {
        return None;
    }

    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            let _ = tx.send(input);
        }
    });

    rx.recv_timeout(Duration::from_secs(timeout_secs))
        .ok()
        .map(|line| line.trim().to_string())
}
