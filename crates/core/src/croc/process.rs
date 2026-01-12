//! Croc process management.

use super::{
    executable::find_croc_executable,
    options::CrocOptions,
    output::{detect_completion, detect_error, parse_code, parse_progress, Progress},
};
use crate::error::{Error, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

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

        // Add global args (like --debug) before subcommand
        for arg in options.to_global_args() {
            cmd.arg(arg);
        }

        cmd.arg("send");

        // Add subcommand-specific options
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
        output_dir: Option<&Path>,
    ) -> Result<(Self, CrocProcessHandle)> {
        let croc_path = find_croc_executable()?;

        let mut cmd = Command::new(&croc_path);

        // Add global args (like --debug) before the code
        for arg in options.to_global_args() {
            cmd.arg(arg);
        }

        // Add receive-specific options as global args (--yes, --overwrite, --out are global)
        for arg in options.to_receive_args() {
            cmd.arg(arg);
        }

        // Set output directory using --out flag (not current_dir)
        if let Some(dir) = output_dir {
            cmd.arg("--out");
            cmd.arg(dir);
        }

        // Just pass the code - croc automatically knows it's a receive operation
        // There is no "receive" subcommand in croc!
        cmd.arg(code);

        Self::spawn(cmd).await
    }

    /// Spawn the croc process and set up output monitoring.
    async fn spawn(mut cmd: Command) -> Result<(Self, CrocProcessHandle)> {
        // Configure process stdio
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // On Windows, we need special handling to prevent croc from waiting on stdin.
        // We use DETACHED_PROCESS to create a new process group, which helps with
        // stdin handling while still allowing network functionality.
        // Note: CREATE_NO_WINDOW (0x08000000) breaks croc's local network discovery.
        #[cfg(windows)]
        {
            // DETACHED_PROCESS = 0x00000008
            // Creates a new process group without a console, but preserves networking.
            const DETACHED_PROCESS: u32 = 0x00000008;
            cmd.creation_flags(DETACHED_PROCESS);
            cmd.stdin(Stdio::null());
        }

        #[cfg(not(windows))]
        {
            cmd.stdin(Stdio::null());
        }

        // Kill croc process when dropped (prevents orphan processes)
        cmd.kill_on_drop(true);

        info!("Spawning croc: {:?}", cmd);

        let mut child = cmd.spawn().map_err(|e| {
            error!("Failed to spawn croc: {}", e);
            Error::CrocProcess(format!("Failed to spawn: {}", e))
        })?;

        // Set up channels for events
        let (event_tx, event_rx) = mpsc::channel(100);
        let (handle_tx, handle_rx) = mpsc::channel(100);

        // Take stdout/stderr for monitoring
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Send started event
        let _ = event_tx.send(CrocEvent::Started).await;
        let _ = handle_tx.send(CrocEvent::Started).await;

        // Spawn task to monitor stdout - use CR/LF aware reading for progress
        if let Some(stdout) = stdout {
            let tx = event_tx.clone();
            let handle_tx = handle_tx.clone();
            tokio::spawn(async move {
                read_lines_cr_lf(stdout, tx, handle_tx, false).await;
            });
        }

        // Spawn task to monitor stderr - use CR/LF aware reading for progress
        if let Some(stderr) = stderr {
            let tx = event_tx.clone();
            let handle_tx = handle_tx;
            tokio::spawn(async move {
                read_lines_cr_lf(stderr, tx, handle_tx, true).await;
            });
        }

        let process = Self { child, event_rx };

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
        self.child
            .kill()
            .await
            .map_err(|e| Error::CrocProcess(format!("Failed to kill process: {}", e)))
    }

    /// Wait for the process to exit.
    pub async fn wait(&mut self) -> Result<std::process::ExitStatus> {
        self.child
            .wait()
            .await
            .map_err(|e| Error::CrocProcess(format!("Failed to wait for process: {}", e)))
    }

    /// Check if the process is still running.
    pub fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        self.child
            .try_wait()
            .map_err(|e| Error::CrocProcess(format!("Failed to check process status: {}", e)))
    }
}

impl CrocProcessHandle {
    /// Wait for the next event.
    pub async fn next_event(&mut self) -> Option<CrocEvent> {
        self.events.recv().await
    }
}

/// Read lines from a reader, treating both \r and \n as line terminators.
/// This is necessary because croc uses \r for progress updates.
async fn read_lines_cr_lf<R: AsyncReadExt + Unpin>(
    mut reader: R,
    tx: mpsc::Sender<CrocEvent>,
    handle_tx: mpsc::Sender<CrocEvent>,
    is_stderr: bool,
) {
    let stream_name = if is_stderr { "stderr" } else { "stdout" };
    info!("Starting to read from {}", stream_name);

    let mut line_buffer = String::with_capacity(256);
    let mut read_buf = [0u8; 4096]; // Read in chunks to avoid blocking
    let mut total_bytes_read: u64 = 0;
    let mut read_count: u64 = 0;

    loop {
        // Read available data - this won't block waiting for a specific amount
        debug!("{}: Waiting for read...", stream_name);
        match reader.read(&mut read_buf).await {
            Ok(0) => {
                info!(
                    "{}: EOF after {} bytes in {} reads",
                    stream_name, total_bytes_read, read_count
                );
                break;
            }
            Ok(n) => {
                total_bytes_read += n as u64;
                read_count += 1;
                debug!(
                    "{}: Read {} bytes (total: {})",
                    stream_name, n, total_bytes_read
                );

                // Process the bytes we read, filtering out non-printable chars except newlines
                for &byte in &read_buf[..n] {
                    let ch = byte as char;
                    if ch == '\r' || ch == '\n' {
                        let line = line_buffer.trim();
                        if !line.is_empty() {
                            process_line(line, &tx, &handle_tx, is_stderr).await;
                        }
                        line_buffer.clear();
                    } else if (32..127).contains(&byte) {
                        // Only keep printable ASCII characters
                        line_buffer.push(ch);
                    }
                    // Skip ANSI escape codes and other control characters
                }
            }
            Err(e) => {
                error!("{}: Error reading: {}", stream_name, e);
                break;
            }
        }
    }

    // Process any remaining data
    let line = line_buffer.trim();
    if !line.is_empty() {
        info!("{}: Processing remaining buffer: {}", stream_name, line);
        process_line(line, &tx, &handle_tx, is_stderr).await;
    }

    info!("{}: Reader finished", stream_name);
}

/// Process a single line and emit appropriate events.
async fn process_line(
    line: &str,
    tx: &mpsc::Sender<CrocEvent>,
    handle_tx: &mpsc::Sender<CrocEvent>,
    is_stderr: bool,
) {
    if is_stderr {
        debug!("croc stderr: {}", line);
    } else {
        debug!("croc stdout: {}", line);
    }

    let _ = tx.send(CrocEvent::Output(line.to_string())).await;
    let _ = handle_tx.send(CrocEvent::Output(line.to_string())).await;

    // Parse the line for events
    if let Some(code) = parse_code(line) {
        let _ = tx.send(CrocEvent::CodeReady(code.clone())).await;
        let _ = handle_tx.send(CrocEvent::CodeReady(code)).await;
    }

    if let Some(progress) = parse_progress(line) {
        let _ = tx.send(CrocEvent::Progress(progress.clone())).await;
        let _ = handle_tx.send(CrocEvent::Progress(progress)).await;
    }

    if detect_completion(line) {
        // Send a 100% progress event first, then completion
        // This ensures the UI shows full progress before marking complete
        let final_progress = Progress {
            percentage: 100.0,
            speed: String::new(),
            current_file: None,
            bytes_transferred: None,
            total_bytes: None,
        };
        let _ = tx.send(CrocEvent::Progress(final_progress.clone())).await;
        let _ = handle_tx.send(CrocEvent::Progress(final_progress)).await;
        let _ = tx.send(CrocEvent::Completed).await;
        let _ = handle_tx.send(CrocEvent::Completed).await;
    }

    if let Some(error) = detect_error(line) {
        let _ = tx.send(CrocEvent::Failed(error.clone())).await;
        let _ = handle_tx.send(CrocEvent::Failed(error)).await;
    }
}
