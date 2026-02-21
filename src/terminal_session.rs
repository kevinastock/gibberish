use anyhow::{Context, Result, bail};
use avt::Vt;
use pty_process::Size;
use pty_process::blocking::{Command as PtyCommand, Pty, open};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal, kill_process_group};
use std::io::{self, ErrorKind, Read, Write};
use std::process::Child;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

use crate::config::SessionConfig;

const WORKER_TICK: Duration = Duration::from_millis(15);
const SHUTDOWN_POLL_TICK: Duration = Duration::from_millis(20);
const SHUTDOWN_TERM_GRACE: Duration = Duration::from_millis(400);
const SHUTDOWN_KILL_GRACE: Duration = Duration::from_millis(400);

enum SessionCommand {
    SendInput(Vec<u8>, oneshot::Sender<Result<()>>),
    Snapshot(oneshot::Sender<Result<TerminalSnapshot>>),
    Reset(oneshot::Sender<Result<()>>),
    Shutdown(oneshot::Sender<Result<()>>),
}

#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub cols: usize,
    pub rows: usize,
    pub cursor: Option<(usize, usize)>,
    pub lines: Vec<String>,
}

impl TerminalSnapshot {
    pub fn render(&self) -> String {
        let mut rendered_lines: Vec<String> = self
            .lines
            .iter()
            .map(|line| line.trim_end().to_string())
            .collect();

        let footer = if let Some((col, row)) = self.cursor {
            let cursor_char = self
                .lines
                .get(row)
                .and_then(|line| line.chars().nth(col))
                .unwrap_or(' ');

            if row >= rendered_lines.len() {
                rendered_lines.resize(row + 1, String::new());
            }

            let line = &mut rendered_lines[row];
            let mut chars: Vec<char> = line.chars().collect();
            if col >= chars.len() {
                chars.resize(col + 1, ' ');
            }
            chars[col] = '▮';
            *line = chars.into_iter().collect();

            format!(
                "Cursor info: row={row}, col={col}, char=\"{}\"",
                escape_display_char(cursor_char)
            )
        } else {
            "Cursor info: row=-, col=-, char=\"\"".to_string()
        };

        let mut rendered = rendered_lines.join("\n");
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(&footer);
        rendered
    }
}

fn escape_display_char(ch: char) -> String {
    ch.escape_default().collect()
}

pub struct TerminalSession {
    cmd_tx: mpsc::Sender<SessionCommand>,
    worker: Option<thread::JoinHandle<Result<()>>>,
}

#[derive(Clone)]
pub struct TerminalSessionHandle {
    cmd_tx: mpsc::Sender<SessionCommand>,
}

impl TerminalSession {
    pub async fn start(options: SessionConfig) -> Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = oneshot::channel();

        let worker = thread::Builder::new()
            .name("pty-vt-worker".to_string())
            .spawn(move || run_worker(options, cmd_rx, ready_tx))
            .context("failed to spawn PTY worker thread")?;

        match ready_rx.await {
            Ok(Ok(())) => Ok(Self {
                cmd_tx,
                worker: Some(worker),
            }),
            Ok(Err(err)) => {
                let _ = join_worker(worker).await;
                Err(err).context("failed to initialize terminal session")
            }
            Err(_) => {
                let _ = join_worker(worker).await;
                bail!("terminal worker exited before initialization")
            }
        }
    }

    pub async fn snapshot(&self) -> Result<TerminalSnapshot> {
        self.handle().snapshot().await
    }

    pub async fn reset(&self) -> Result<()> {
        self.handle().reset().await
    }

    pub fn handle(&self) -> TerminalSessionHandle {
        TerminalSessionHandle {
            cmd_tx: self.cmd_tx.clone(),
        }
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };

        let (ack_tx, ack_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(SessionCommand::Shutdown(ack_tx));
        let _ = tokio::time::timeout(Duration::from_secs(1), ack_rx).await;

        join_worker(worker).await
    }
}

impl TerminalSessionHandle {
    pub async fn send_input(&self, bytes: impl AsRef<[u8]>) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::SendInput(bytes.as_ref().to_vec(), ack_tx))
            .context("terminal worker is not running")?;

        ack_rx
            .await
            .context("terminal worker dropped input acknowledgement")?
    }

    pub async fn snapshot(&self) -> Result<TerminalSnapshot> {
        let (snap_tx, snap_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::Snapshot(snap_tx))
            .context("terminal worker is not running")?;

        snap_rx
            .await
            .context("terminal worker dropped snapshot response")?
    }

    pub async fn reset(&self) -> Result<()> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::Reset(ack_tx))
            .context("terminal worker is not running")?;

        ack_rx
            .await
            .context("terminal worker dropped reset acknowledgement")?
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.worker.is_none() {
            return;
        }

        let (ack_tx, _ack_rx) = oneshot::channel();
        let _ = self.cmd_tx.send(SessionCommand::Shutdown(ack_tx));

        // Avoid blocking a Tokio runtime worker thread during drop.
        let _ = self.worker.take();
    }
}

fn run_worker(
    options: SessionConfig,
    cmd_rx: Receiver<SessionCommand>,
    ready_tx: oneshot::Sender<Result<()>>,
) -> Result<()> {
    let (cols, rows) = options.terminal_size()?;
    let cols_u16 = u16::try_from(cols).context("terminal columns exceed u16 range")?;
    let rows_u16 = u16::try_from(rows).context("terminal rows exceed u16 range")?;

    let setup = spawn_terminal_parts(&options, cols, rows, cols_u16, rows_u16);
    let (mut pty, mut child, mut vt) = match setup {
        Ok(parts) => {
            let _ = ready_tx.send(Ok(()));
            parts
        }
        Err(err) => {
            let _ = ready_tx.send(Err(err));
            return Ok(());
        }
    };

    let mut read_buf = [0_u8; 8192];
    let mut child_exited = false;
    let mut running = true;

    while running {
        if !child_exited {
            match drain_pty_output(&mut pty, &mut vt, &mut read_buf) {
                Ok(is_eof) => {
                    if is_eof {
                        child_exited = true;
                    }
                }
                Err(err) => return Err(err).context("failed to process PTY output"),
            }

            if let Some(_status) = child.try_wait().context("failed to poll bash process")? {
                child_exited = true;
            }
        }

        match cmd_rx.recv_timeout(WORKER_TICK) {
            Ok(SessionCommand::SendInput(bytes, ack)) => {
                let res = if child_exited {
                    Err(anyhow::anyhow!("bash process has already exited"))
                } else {
                    write_all_with_retry(&mut pty, &bytes).context("failed to write to PTY")
                };
                let _ = ack.send(res);
            }
            Ok(SessionCommand::Snapshot(reply)) => {
                if !child_exited {
                    let _ = drain_pty_output(&mut pty, &mut vt, &mut read_buf);
                }

                let snapshot = TerminalSnapshot {
                    cols,
                    rows,
                    cursor: vt.cursor().into(),
                    lines: vt.view().map(|line| line.text()).collect(),
                };
                let _ = reply.send(Ok(snapshot));
            }
            Ok(SessionCommand::Reset(ack)) => {
                let res = (|| -> Result<()> {
                    let (new_pty, new_child, new_vt) =
                        spawn_terminal_parts(&options, cols, rows, cols_u16, rows_u16)?;
                    terminate_bash_and_children(&mut child);
                    pty = new_pty;
                    child = new_child;
                    vt = new_vt;
                    child_exited = false;
                    Ok(())
                })();
                let _ = ack.send(res);
            }
            Ok(SessionCommand::Shutdown(ack)) => {
                let _ = ack.send(Ok(()));
                running = false;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => running = false,
        }
    }

    terminate_bash_and_children(&mut child);
    Ok(())
}

fn spawn_terminal_parts(
    options: &SessionConfig,
    cols: usize,
    rows: usize,
    cols_u16: u16,
    rows_u16: u16,
) -> Result<(Pty, Child, Vt)> {
    let (pty, pts) = open().context("failed to open PTY master")?;
    set_pty_nonblocking(&pty).context("failed to set PTY nonblocking mode")?;
    pty.resize(Size::new(rows_u16, cols_u16))
        .context("failed to resize PTY")?;
    let child = PtyCommand::new(&options.shell.program)
        // `pty-process` defaults to canonical (cooked) mode, which is what we want:
        // control bytes like Ctrl-C/Ctrl-Z become terminal-generated signals.
        .args(&options.shell.args)
        .envs(&options.shell.env)
        .spawn(pts)
        .context("failed to spawn bash process")?;
    let vt = Vt::builder().size(cols, rows).scrollback_limit(0).build();
    Ok((pty, child, vt))
}

async fn join_worker(worker: thread::JoinHandle<Result<()>>) -> Result<()> {
    let joined = tokio::task::spawn_blocking(move || worker.join())
        .await
        .map_err(|err| anyhow::anyhow!("failed to join terminal worker thread: {err}"))?;

    match joined {
        Ok(result) => result,
        Err(_) => bail!("terminal worker panicked"),
    }
}

fn set_pty_nonblocking(pty: &Pty) -> io::Result<()> {
    let mut flags = fcntl_getfl(pty).map_err(io::Error::from)?;
    flags |= OFlags::NONBLOCK;
    fcntl_setfl(pty, flags).map_err(io::Error::from)
}

fn drain_pty_output(pty: &mut Pty, vt: &mut Vt, read_buf: &mut [u8]) -> io::Result<bool> {
    loop {
        match pty.read(read_buf) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&read_buf[..n]);
                vt.feed_str(&chunk);
            }
            Err(err) if err.raw_os_error() == Some(libc::EIO) => return Ok(true),
            Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(false),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
}

fn write_all_with_retry(pty: &mut Pty, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match pty.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::WriteZero,
                    "write returned zero bytes",
                ));
            }
            Ok(n) => bytes = &bytes[n..],
            Err(err) => {
                if err.kind() == ErrorKind::Interrupted || err.kind() == ErrorKind::WouldBlock {
                    thread::sleep(Duration::from_millis(5));
                } else {
                    return Err(err);
                }
            }
        }
    }

    Ok(())
}

fn terminate_bash_and_children(child: &mut Child) {
    let process_group = Pid::from_child(child);

    if wait_for_child_exit(child, Duration::ZERO).unwrap_or(false) {
        return;
    }

    let _ = signal_process_group(process_group, Signal::TERM);
    if wait_for_child_exit(child, SHUTDOWN_TERM_GRACE).unwrap_or(false) {
        return;
    }

    let _ = signal_process_group(process_group, Signal::KILL);
    if wait_for_child_exit(child, SHUTDOWN_KILL_GRACE).unwrap_or(false) {
        return;
    }

    let _ = child.kill();
    let _ = wait_for_child_exit(child, SHUTDOWN_KILL_GRACE);
}

fn signal_process_group(process_group: Pid, signal: Signal) -> io::Result<()> {
    match kill_process_group(process_group, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;

    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(false);
        }

        thread::sleep((deadline - now).min(SHUTDOWN_POLL_TICK));
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalSnapshot;

    #[test]
    fn render_replaces_cursor_and_adds_footer() {
        let snapshot = TerminalSnapshot {
            cols: 10,
            rows: 2,
            cursor: Some((1, 0)),
            lines: vec!["abc".to_string(), "xyz".to_string()],
        };

        assert_eq!(
            snapshot.render(),
            "a▮c\nxyz\nCursor info: row=0, col=1, char=\"b\""
        );
    }

    #[test]
    fn render_handles_cursor_on_trimmed_trailing_space() {
        let snapshot = TerminalSnapshot {
            cols: 10,
            rows: 1,
            cursor: Some((4, 0)),
            lines: vec!["ab   ".to_string()],
        };

        assert_eq!(
            snapshot.render(),
            "ab  ▮\nCursor info: row=0, col=4, char=\" \""
        );
    }

    #[test]
    fn render_escapes_cursor_char_in_footer() {
        let snapshot = TerminalSnapshot {
            cols: 10,
            rows: 1,
            cursor: Some((0, 0)),
            lines: vec!["\"".to_string()],
        };

        assert_eq!(
            snapshot.render(),
            "▮\nCursor info: row=0, col=0, char=\"\\\"\""
        );
    }

    #[test]
    fn render_handles_hidden_cursor() {
        let snapshot = TerminalSnapshot {
            cols: 10,
            rows: 1,
            cursor: None,
            lines: vec!["abc".to_string()],
        };

        assert_eq!(
            snapshot.render(),
            "abc\nCursor info: row=-, col=-, char=\"\""
        );
    }
}
