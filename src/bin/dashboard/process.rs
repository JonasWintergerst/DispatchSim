use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};

/// Which subprocess the dashboard launched. Used by `poll_process` to route
/// stdout to the right parser and to decide what to do on exit (e.g. only the
/// `Simulate` kind should auto-generate a report after Stop).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProcessKind {
    Optimize,
    Simulate,
    PatrolGen,
    WhatIf,
    WhatIfPatrol,
}

pub struct RunningProcess {
    pub kind: ProcessKind,
    pub child: Child,
    pub stdout_rx: Receiver<String>,
}

/// Spawn a command with piped stdout; a background thread forwards each line
/// through the returned channel so the UI thread never blocks.
pub fn spawn_with_live_stdout(
    mut cmd: Command,
    kind: ProcessKind,
) -> Result<RunningProcess, std::io::Error> {
    let mut child = cmd.stdout(Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for l in reader.lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    Ok(RunningProcess { kind, child, stdout_rx: rx })
}
