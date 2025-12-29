//! Croc process management.

use super::{
    executable::find_croc_executable,
    options::CrocOptions,
    output::{detect_completion, detect_error, parse_code, parse_progress, Progress},
};
use crate::error::{Error, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Events emitted by a croc process.
#[derive(Debug, Clone)]
pub enum CrocEvent {
    /// Croc code available (send only).
    CodeReady(String),
    /// Progress update.
    Progress(Progress),
    /// Transfer completed successfully.
    Completed,
    /// Transfer failed with error.
    Failed(String),
    /// Raw output line (for debugging).
    Output(String),
    /// Process started.
    Started,
    /// Waiting for receiver.
    Waiting,
}

/// A running croc process.
pub struct CrocProcess {
    child: Child,
    event_rx: mpsc::Receiver<CrocEvent>,
}

/// Handle to control a croc process.
pub struct CrocProcessHandle {
    /// Channel to receive events.
    pub events: mpsc::Receiver<CrocEvent>,
}

impl CrocProcess {
    /// Start a croc send process.
    pub async fn send(
        files: &[impl AsRef<Path>],
        options: &CrocOptions,
    ) -> Result<(Self, CrocProcessHandle)> {
        let croc_path = find_croc_executable()?;

        let mut cmd = Command::new(&croc_path);
        cmd.arg("send");

        // Add options
        for arg in options.to_send_args() {
            cmd.arg(arg);
        }

        // Add files
        for file in files {
            cmd.arg(file.as_ref());
        }

        Self::spawn(cmd).await
    }

    /// Start a croc receive process.
    pub async fn receive(
        code: &str,
        options: &CrocOptions,
        working_dir: Option<&Path>,
    ) -> Result<(Self, CrocProcessHandle)> {
        let croc_path = find_croc_executable()?;

        let mut cmd = Command::new(&croc_path);
        
        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }
        
        cmd.arg("--yes"); // Auto-accept
        
        // Add options
        for arg in options.to_receive_args() {
            cmd.arg(arg);
        }
        
        cmd.arg(code);

        Self::spawn(cmd).await
    }

    /// Spawn the croc process and set up output monitoring.
    async fn spawn(mut cmd: Command) -> Result<(Self, CrocProcessHandle)> {
        // Configure process
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        info!("Spawning croc: {:?}", cmd);

        let mut child = cmd.spawn().map_err(|e| {
            error!("Failed to spawn croc: {}", e);
            Error::CrocProcess(format!("Failed to spawn: {}", e))
        })?;

        // Set up channels for events
        let (event_tx, event_rx) = mpsc::channel(100);
        let (handle_tx, handle_rx) = mpsc::channel(100);

        // Take stdout/stderr
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Send started event
        let _ = event_tx.send(CrocEvent::Started).await;
        let _ = handle_tx.send(CrocEvent::Started).await;

        // Spawn task to monitor stdout
        if let Some(stdout) = stdout {
            let tx = event_tx.clone();
            let handle_tx = handle_tx.clone();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();

                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("croc stdout: {}", line);
                    let _ = tx.send(CrocEvent::Output(line.clone())).await;
                    let _ = handle_tx.send(CrocEvent::Output(line.clone())).await;

                    // Parse the line for events
                    if let Some(code) = parse_code(&line) {
                        let _ = tx.send(CrocEvent::CodeReady(code.clone())).await;
                        let _ = handle_tx.send(CrocEvent::CodeReady(code)).await;
                    }

                    if let Some(progress) = parse_progress(&line) {
                        let _ = tx.send(CrocEvent::Progress(progress.clone())).await;
                        let _ = handle_tx.send(CrocEvent::Progress(progress)).await;
                    }

                    if detect_completion(&line) {
                        let _ = tx.send(CrocEvent::Completed).await;
                        let _ = handle_tx.send(CrocEvent::Completed).await;
                    }

                    if let Some(error) = detect_error(&line) {
                        let _ = tx.send(CrocEvent::Failed(error.clone())).await;
                        let _ = handle_tx.send(CrocEvent::Failed(error)).await;
                    }
                }
            });
        }

        // Spawn task to monitor stderr
        if let Some(stderr) = stderr {
            let tx = event_tx.clone();
            let handle_tx = handle_tx;
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();

                while let Ok(Some(line)) = lines.next_line().await {
                    debug!("croc stderr: {}", line);
                    
                    // Stderr often contains progress for croc
                    if let Some(progress) = parse_progress(&line) {
                        let _ = tx.send(CrocEvent::Progress(progress.clone())).await;
                        let _ = handle_tx.send(CrocEvent::Progress(progress)).await;
                    }

                    if let Some(code) = parse_code(&line) {
                        let _ = tx.send(CrocEvent::CodeReady(code.clone())).await;
                        let _ = handle_tx.send(CrocEvent::CodeReady(code)).await;
                    }

                    if let Some(error) = detect_error(&line) {
                        let _ = tx.send(CrocEvent::Failed(error.clone())).await;
                        let _ = handle_tx.send(CrocEvent::Failed(error)).await;
                    }

                    if detect_completion(&line) {
                        let _ = tx.send(CrocEvent::Completed).await;
                        let _ = handle_tx.send(CrocEvent::Completed).await;
                    }
                }
            });
        }

        let process = Self {
            child,
            event_rx,
        };

        let handle = CrocProcessHandle { events: handle_rx };

        Ok((process, handle))
    }

    /// Wait for the next event.
    pub async fn next_event(&mut self) -> Option<CrocEvent> {
        self.event_rx.recv().await
    }

    /// Kill the process.
    pub async fn kill(&mut self) -> Result<()> {
        warn!("Killing croc process");
        self.child.kill().await.map_err(|e| {
            Error::CrocProcess(format!("Failed to kill process: {}", e))
        })
    }

    /// Wait for the process to exit.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child.wait().await.map_err(|e| {
            Error::CrocProcess(format!("Failed to wait for process: {}", e))
        })
    }

    /// Check if the process is still running.
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child.try_wait().map_err(|e| {
            Error::CrocProcess(format!("Failed to check process status: {}", e))
        })
    }
}

impl CrocProcessHandle {
    /// Wait for the next event.
    pub async fn next_event(&mut self) -> Option<CrocEvent> {
        self.events.recv().await
    }
}

