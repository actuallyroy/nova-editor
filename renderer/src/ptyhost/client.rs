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
}

pub struct Client {
    conn: Conn,
    rx: Receiver<Frame>,
}

impl Client {
    /// Connect to the running daemon, or spawn one (detached) and connect. Returns
    /// the client plus the daemon's current terminals (to re-attach on launch).
    pub fn connect_or_spawn() -> Option<(Client, Vec<TermInfo>)> {
        if let Some(c) = try_connect() {
            return Some(c);
        }
        spawn_daemon();
        // The daemon needs a moment to bind + write its discovery file.
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(50));
            if let Some(c) = try_connect() {
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
    #[cfg(test)]
    pub fn write(&self, id: TermId, data: &[u8]) {
        send(&self.conn, Frame::Write { id, data: data.to_vec() });
    }

    /// Drain queued frames into GUI-facing events. Non-blocking.
    pub fn poll(&self) -> Vec<Incoming> {
        let mut out = Vec::new();
        while let Ok(frame) = self.rx.try_recv() {
            match frame {
                Frame::Control(Msg::Created { id, title }) => out.push(Incoming::Created { id, title }),
                Frame::Control(Msg::Backlog { id, data }) => out.push(Incoming::Backlog { id, data }),
                Frame::Output { id, data } => out.push(Incoming::Output { id, data }),
                Frame::Control(Msg::Exited { id }) => out.push(Incoming::Exited { id }),
                _ => {}
            }
        }
        out
    }
}

/// Read the discovery file, connect, authenticate, and start the reader thread.
fn try_connect() -> Option<(Client, Vec<TermInfo>)> {
    let path = super::info_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let info: HostInfo = serde_json::from_str(&text).ok()?;
    let mut stream = TcpStream::connect(("127.0.0.1", info.port)).ok()?;
    stream.set_nodelay(true).ok();
    // Bounded handshake so a stale file (dead daemon on a reused port) doesn't hang.
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    Frame::Control(Msg::Hello { token: info.token }).write_to(&mut stream).ok()?;
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
    Some((Client { conn: Arc::new(Mutex::new(stream)), rx }, terminals))
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
    use std::io::Write as _;

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
            let mut s = TcpStream::connect(("127.0.0.1", info.port)).ok()?;
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
        Frame::Control(Msg::Hello { token: token.clone() }).write_to(&mut a).unwrap();
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

        // --- Client A drops (GUI killed); Client B reconnects ---
        a.shutdown(std::net::Shutdown::Both).ok();
        drop(a);
        let (mut b, token2) = connect(50).expect("reconnect");
        Frame::Control(Msg::Hello { token: token2 }).write_to(&mut b).unwrap();
        match Frame::read_from(&mut b) {
            Ok(Frame::Control(Msg::Welcome { terminals })) => {
                assert!(!terminals.is_empty(), "surviving shell must be listed on reconnect");
            }
            other => panic!("expected Welcome on reconnect, got {other:?}"),
        }
    }
}
