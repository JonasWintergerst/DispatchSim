use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};

pub struct RunningProcess {
    pub child: Child,
    pub stdout_rx: Receiver<String>,
}

/// Spawn a command with piped stdout; a background thread forwards each line
/// through the returned channel so the UI thread never blocks.
pub fn spawn_with_live_stdout(mut cmd: Command) -> Result<RunningProcess, std::io::Error> {
    let mut child = cmd.stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(l) = line {
                let _ = tx.send(l);
            }
        }
    });
    Ok(RunningProcess { child, stdout_rx: rx })
}

/// Keep lines that carry per-iteration progress; skip noise / cargo build output.
pub fn is_progress_line(line: &str) -> bool {
    if line.starts_with("Station ") && line.contains("selected") {
        return true;
    }
    if line.starts_with("event ") && line.contains("sim time") {
        return true;
    }
    false
}
