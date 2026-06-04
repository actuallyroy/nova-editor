// Pty-host client used by the GUI. Connects to the daemon (spawning it detached if
// it isn't running), authenticates, and exposes a small API plus a `poll()` that
// drains incoming output/control frames. All terminals share one connection; the
// panel routes frames to panes by `TermId`. The write half is an `Arc<Mutex<_>>`
// (`Conn`) so each `Terminal` can send keystrokes/resizes without prop-drilling.

use std::io::{self};
use std::net::TcpStream;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{Frame, HostInfo, Msg, TermId, TermInfo};

/// Shared write half of the daemon connection — cloned into every `Terminal`.
pub type Conn = Arc<Mutex<TcpStream>>;

/// Send one frame over a shared connection (best-effort).
pub fn send(conn: &Conn, frame: Frame) {
    if let Ok(mut s) = conn.lock() {
        let _ = frame.write_to(&mut *s);
    }
}

/// What the GUI consumes from `poll()` (control noise like Pong/Welcome filtered).
pub enum Incoming {
    Created { id: TermId, title: String },
    Backlog { id: TermId, data: Vec<u8> },
    Output { id: TermId, data: Vec<u8> },
    Exited { id: TermId },
    /// Another instance opened this window's workspace — raise this window.
    Focus,
    /// Orphaned shells offered after a workspace switch (`SetWorkspace` reply) —
    /// the just-opened folder's terminals, restorable when the panel opens.
    Offered(Vec<TermInfo>),
}

pub struct Client {
    conn: Conn,
    rx: Receiver<Frame>,
    /// Frames set aside by a blocking request (`focus_existing`) so they still reach
    /// `poll()` afterwards in order.
    stash: std::collections::VecDeque<Frame>,
}

impl Client {
    /// Connect to the running daemon, or spawn one (detached) and connect. Returns
    /// the client plus the daemon's current terminals (to re-attach on launch).
    pub fn connect_or_spawn(workspace: &str) -> Option<(Client, Vec<TermInfo>)> {
        if let Some(c) = try_connect(workspace) {
            return Some(c);
        }
        spawn_daemon();
        // The daemon needs a moment to bind + write its discovery file.
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(50));
            if let Some(c) = try_connect(workspace) {
                return Some(c);
            }
        }
        None
    }

    /// The shared write handle, for `Terminal`s to send keystrokes/resizes.
    pub fn conn(&self) -> Conn {
        self.conn.clone()
    }

    pub fn create(&self, cwd: &str, rows: u16, cols: u16) {
        send(&self.conn, Frame::Control(Msg::Create { cwd: cwd.to_string(), rows, cols }));
    }
    pub fn attach(&self, id: TermId) {
        send(&self.conn, Frame::Control(Msg::Attach { id }));
    }
    pub fn close(&self, id: TermId) {
        send(&self.conn, Frame::Control(Msg::Close { id }));
    }
    /// Release a terminal back to the daemon (kept running, reclaimable later).
    pub fn detach(&self, id: TermId) {
        send(&self.conn, Frame::Control(Msg::Detach { id }));
    }
    /// Update this window's registered workspace after Open Folder.
    pub fn set_workspace(&self, workspace: &str) {
        send(&self.conn, Frame::Control(Msg::SetWorkspace { workspace: workspace.to_string() }));
    }
    /// How many of this window's shells are running a foreground process. Blocking
    /// round-trip (bounded); unrelated frames are stashed for `poll`.
    pub fn busy_count(&mut self) -> usize {
        send(&self.conn, Frame::Control(Msg::QueryBusy));
        let deadline = std::time::Instant::now() + Duration::from_millis(600);
        while std::time::Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Frame::Control(Msg::BusyResult { count })) => return count,
                Ok(other) => self.stash.push_back(other),
                Err(_) => {}
            }
        }
        0 // no reply — don't block the close on a wedged daemon
    }

    /// Is `id`'s shell running a foreground process? Blocking round-trip (bounded);
    /// unrelated frames are stashed for `poll`. Defaults to BUSY on no reply, so a
    /// wedged daemon never turns a TUI's ESC into a destructive kill-line.
    pub fn term_busy(&mut self, id: TermId) -> bool {
        send(&self.conn, Frame::Control(Msg::QueryTermBusy { id }));
        let deadline = std::time::Instant::now() + Duration::from_millis(300);
        while std::time::Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Frame::Control(Msg::TermBusyResult { id: rid, busy })) if rid == id => return busy,
                Ok(other) => self.stash.push_back(other),
                Err(_) => {}
            }
        }
        true
    }

    /// Ask the daemon to focus another live window that already has `workspace`
    /// open. Returns true if one was found (so the caller should NOT open the folder
    /// here). Waits briefly for the reply; unrelated frames are stashed for `poll`.
    pub fn focus_existing(&mut self, workspace: &str) -> bool {
        send(&self.conn, Frame::Control(Msg::FocusWindow { workspace: workspace.to_string() }));
        let deadline = std::time::Instant::now() + Duration::from_millis(800);
        while std::time::Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Frame::Control(Msg::FocusResult { found })) => return found,
                Ok(other) => self.stash.push_back(other),
                Err(_) => {}
            }
        }
        false // no reply — behave as if not open elsewhere
    }

    /// Drain queued frames into GUI-facing events. Non-blocking.
    pub fn poll(&mut self) -> Vec<Incoming> {
        let mut out = Vec::new();
        let mut handle = |frame: Frame| match frame {
            Frame::Control(Msg::Created { id, title }) => out.push(Incoming::Created { id, title }),
            Frame::Control(Msg::Backlog { id, data }) => out.push(Incoming::Backlog { id, data }),
            Frame::Output { id, data } => out.push(Incoming::Output { id, data }),
            Frame::Control(Msg::Exited { id }) => out.push(Incoming::Exited { id }),
            Frame::Control(Msg::Focus) => out.push(Incoming::Focus),
            // The handshake consumes the initial Welcome, so one seen here is the
            // re-offer that follows a SetWorkspace (Open Folder).
            Frame::Control(Msg::Welcome { terminals }) => out.push(Incoming::Offered(terminals)),
            _ => {}
        };
        while let Some(f) = self.stash.pop_front() {
            handle(f);
        }
        while let Ok(frame) = self.rx.try_recv() {
            handle(frame);
        }
        out
    }
}

/// Read the discovery file, connect, authenticate (declaring the window's workspace
/// so re-attach is scoped to it), and start the reader thread.
fn try_connect(workspace: &str) -> Option<(Client, Vec<TermInfo>)> {
    let path = super::info_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let info: HostInfo = serde_json::from_str(&text).ok()?;
    let mut stream = TcpStream::connect(("127.0.0.1", info.port)).ok()?;
    stream.set_nodelay(true).ok();
    // Bounded handshake so a stale file (dead daemon on a reused port) doesn't hang.
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    Frame::Control(Msg::Hello { token: info.token, workspace: workspace.to_string() })
        .write_to(&mut stream)
        .ok()?;
    let terminals = match Frame::read_from(&mut stream).ok()? {
        Frame::Control(Msg::Welcome { terminals }) => terminals,
        _ => return None,
    };
    // Back to blocking reads for the streaming phase.
    stream.set_read_timeout(None).ok();
    let mut reader = stream.try_clone().ok()?;
    let (tx, rx) = channel();
    std::thread::spawn(move || loop {
        match Frame::read_from(&mut reader) {
            Ok(f) => {
                if tx.send(f).is_err() {
                    break;
                }
            }
            Err(_) => break, // daemon gone / socket closed
        }
    });
    Some((Client { conn: Arc::new(Mutex::new(stream)), rx, stash: std::collections::VecDeque::new() }, terminals))
}

/// Launch `aether --pty-host` fully detached so it outlives this GUI process.
fn spawn_daemon() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--pty-host")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group → detached from our controlling terminal's signals; the
        // parent exiting then leaves it reparented to init, still running.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let _ = cmd.spawn();
}

/// Run the daemon (entry point for the `--pty-host` process mode).
pub fn run_daemon() -> io::Result<()> {
    super::daemon::run()
}

#[cfg(test)]
mod tests {
    use super::*;

    // One daemon, raw sockets, two checks in sequence (a single test so two
    // daemon-spawning tests can't clash on the shared discovery file):
    //   1. echo round-trip — framing + auth + PTY plumbing end to end;
    //   2. reconnect — after the first client drops, a fresh client must still get a
    //      Welcome listing the surviving shell (regression for the connection-
    //      generation race that used to spawn a duplicate daemon). Unix-only.
    #[cfg(unix)]
    #[test]
    fn daemon_echo_and_reconnect() {
        std::thread::spawn(|| {
            let _ = super::super::daemon::run();
        });
        let raw = || -> Option<(TcpStream, String)> {
            let info: HostInfo =
                serde_json::from_str(&std::fs::read_to_string(super::super::info_path()?).ok()?).ok()?;
            let s = TcpStream::connect(("127.0.0.1", info.port)).ok()?;
            s.set_read_timeout(Some(Duration::from_secs(3))).ok();
            Some((s, info.token))
        };
        let connect = |tries: usize| -> Option<(TcpStream, String)> {
            for _ in 0..tries {
                std::thread::sleep(Duration::from_millis(20));
                if let Some(x) = raw() {
                    return Some(x);
                }
            }
            None
        };

        // --- Client A: hello, create a shell, echo a marker ---
        let (mut a, token) = connect(150).expect("daemon up");
        Frame::Control(Msg::Hello { token: token.clone(), workspace: "/tmp".into() }).write_to(&mut a).unwrap();
        assert!(matches!(Frame::read_from(&mut a), Ok(Frame::Control(Msg::Welcome { .. }))));
        Frame::Control(Msg::Create { cwd: "/tmp".into(), rows: 24, cols: 80 }).write_to(&mut a).unwrap();

        let mut id = None;
        let mut acc: Vec<u8> = Vec::new();
        let start = std::time::Instant::now();
        // Get the id, send the echo, then read until the marker comes back.
        while start.elapsed() < Duration::from_secs(8) {
            match Frame::read_from(&mut a) {
                Ok(Frame::Control(Msg::Created { id: i, .. })) => {
                    id = Some(i);
                    Frame::Write { id: i, data: b"echo aether_marker_123\n".to_vec() }.write_to(&mut a).unwrap();
                }
                Ok(Frame::Output { data, .. }) => {
                    acc.extend_from_slice(&data);
                    if acc.windows(17).any(|w| w == b"aether_marker_123") {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(id.is_some(), "shell created");
        assert!(acc.windows(17).any(|w| w == b"aether_marker_123"), "echoed marker streamed back");

        let term_id = id.unwrap();

        // --- Client A drops (GUI killed); Client B reconnects + reclaims ---
        a.shutdown(std::net::Shutdown::Both).ok();
        drop(a);
        let (mut b, token2) = connect(50).expect("reconnect");
        Frame::Control(Msg::Hello { token: token2.clone(), workspace: "/tmp".into() }).write_to(&mut b).unwrap();
        match Frame::read_from(&mut b) {
            Ok(Frame::Control(Msg::Welcome { terminals })) => {
                assert!(terminals.iter().any(|t| t.id == term_id), "orphaned shell listed on reconnect");
            }
            other => panic!("expected Welcome on reconnect, got {other:?}"),
        }
        // B claims it (Attach → owner = B), draining the backlog reply.
        Frame::Control(Msg::Attach { id: term_id }).write_to(&mut b).unwrap();
        assert!(matches!(Frame::read_from(&mut b), Ok(Frame::Control(Msg::Backlog { .. }))));

        // --- Isolation: a concurrent window (same workspace) must NOT see B's shell ---
        let (mut c, token3) = connect(50).expect("third client");
        Frame::Control(Msg::Hello { token: token3, workspace: "/tmp".into() }).write_to(&mut c).unwrap();
        match Frame::read_from(&mut c) {
            Ok(Frame::Control(Msg::Welcome { terminals })) => {
                assert!(
                    terminals.is_empty(),
                    "a second live window must not see another window's claimed terminal"
                );
            }
            other => panic!("expected empty Welcome, got {other:?}"),
        }
        // Keep B alive until here so its ownership holds.
        drop(b);
    }
}
