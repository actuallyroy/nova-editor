// Pty-host: a small daemon that owns the terminal PTYs + shell processes so they
// survive a full restart of the GUI. The GUI is a thin client that re-attaches on
// launch and replays each terminal's output backlog into its own vte parser/grid.
//
// Transport is localhost TCP guarded by a random token written to
// `~/.aether/ptyhost.json` — one code path on every platform (no Unix-socket vs
// named-pipe split). The daemon is the same `aether` binary run as `--pty-host`.
//
// This module is the shared wire protocol + framing; `daemon` and `client` build
// on it.

pub mod client;
pub mod daemon;

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// A terminal handle in the daemon (stable across reconnects).
pub type TermId = u64;

/// Control messages (JSON-encoded). Output bytes travel as a separate binary frame
/// (see `Frame::Output`) so the hot path isn't base64-bloated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Msg {
    /// First message from a client; authenticates with the token file's token and
    /// declares the window's workspace root (so re-attach is scoped to it).
    Hello { token: String, workspace: String },
    /// Daemon's reply to `Hello` — the *orphaned* terminals for this workspace (from
    /// a closed window), which the client may re-claim. Terminals still owned by
    /// another live window are never offered, so windows don't leak into each other.
    Welcome { terminals: Vec<TermInfo> },
    /// Spawn a new shell. Daemon replies with `Created`.
    Create { cwd: String, rows: u16, cols: u16 },
    Created { id: TermId, title: String },
    /// Subscribe to a terminal's output; daemon first sends `Backlog`, then live
    /// `Output` frames.
    Attach { id: TermId },
    /// The recent output buffer for a just-attached terminal (raw VT bytes).
    Backlog { id: TermId, data: Vec<u8> },
    Resize { id: TermId, rows: u16, cols: u16 },
    /// Close (kill) a terminal.
    Close { id: TermId },
    /// Release a terminal without killing it (window switched folders) — it becomes
    /// an orphan, reclaimable by the next window that opens its workspace.
    Detach { id: TermId },
    /// A terminal's shell exited (sent unsolicited).
    Exited { id: TermId },
    /// Ask the daemon to focus the live window that has `workspace` open (single-
    /// window-per-folder, like VSCode). Daemon replies `FocusResult`.
    FocusWindow { workspace: String },
    /// This window switched folders (Open Folder) — update its registry entry.
    SetWorkspace { workspace: String },
    /// How many of this window's shells have a foreground process running (not just
    /// an idle prompt)? Used for the close-window warning. Replies `BusyResult`.
    QueryBusy,
    BusyResult { count: usize },
    FocusResult { found: bool },
    /// Daemon→GUI: another instance asked for this window — raise it.
    Focus,
    /// Liveness check.
    Ping,
    Pong,
}

/// Metadata for one live terminal, used to rebuild tabs on re-attach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TermInfo {
    pub id: TermId,
    pub title: String,
    pub cwd: String,
}

/// A framed message on the wire. Control is JSON; Write/Output carry raw terminal
/// bytes for one terminal id without base64 overhead.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Control(Msg),
    /// Client→daemon keystrokes for a terminal.
    Write { id: TermId, data: Vec<u8> },
    /// Daemon→client shell output for a terminal.
    Output { id: TermId, data: Vec<u8> },
}

// Wire format: [u32 len][u8 tag][payload], len counts tag+payload.
//   tag 0: payload = JSON(Msg)
//   tag 1: payload = [u64 id][raw bytes]   (Write)
//   tag 2: payload = [u64 id][raw bytes]   (Output)
const TAG_CONTROL: u8 = 0;
const TAG_WRITE: u8 = 1;
const TAG_OUTPUT: u8 = 2;
const MAX_FRAME: u32 = 64 * 1024 * 1024; // guard against a corrupt length

impl Frame {
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let (tag, body): (u8, Vec<u8>) = match self {
            Frame::Control(msg) => {
                let json = serde_json::to_vec(msg).map_err(io::Error::other)?;
                (TAG_CONTROL, json)
            }
            Frame::Write { id, data } => (TAG_WRITE, id_prefixed(*id, data)),
            Frame::Output { id, data } => (TAG_OUTPUT, id_prefixed(*id, data)),
        };
        let len = (body.len() as u32) + 1; // +1 for the tag byte
        w.write_all(&len.to_be_bytes())?;
        w.write_all(&[tag])?;
        w.write_all(&body)?;
        w.flush()
    }

    pub fn read_from<R: Read>(r: &mut R) -> io::Result<Frame> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf);
        if len == 0 || len > MAX_FRAME {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad frame length"));
        }
        let mut body = vec![0u8; len as usize];
        r.read_exact(&mut body)?;
        let tag = body[0];
        let payload = &body[1..];
        match tag {
            TAG_CONTROL => {
                let msg = serde_json::from_slice(payload).map_err(io::Error::other)?;
                Ok(Frame::Control(msg))
            }
            TAG_WRITE | TAG_OUTPUT => {
                if payload.len() < 8 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too short"));
                }
                let id = u64::from_be_bytes(payload[..8].try_into().unwrap());
                let data = payload[8..].to_vec();
                Ok(if tag == TAG_WRITE {
                    Frame::Write { id, data }
                } else {
                    Frame::Output { id, data }
                })
            }
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "unknown frame tag")),
        }
    }
}

fn id_prefixed(id: TermId, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + data.len());
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(data);
    v
}

/// Path to the daemon's discovery file, if a config dir is resolvable. The protocol
/// version is part of the FILENAME: old builds (v1 single-client protocol at
/// `ptyhost.json`) and new builds never read each other's files, so a stale install
/// can't capture new windows (and vice versa).
pub fn info_path() -> Option<std::path::PathBuf> {
    // Keyed by the running binary's path: a debug build and the installed release
    // app each get their OWN daemon. Sharing one file made them fight — every GUI
    // that couldn't claim the other's daemon spawned a duplicate, and the
    // single-window-per-folder check forwarded debug launches to a release window
    // (the debug process then exits "silently").
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Ok(exe) = std::env::current_exe() {
        exe.hash(&mut h);
    }
    crate::settings::config_dir().map(|d| d.join(format!("ptyhost-v2-{:08x}.json", h.finish() as u32)))
}

/// Contents of the discovery file: where to connect + the auth token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(f: Frame) {
        let mut buf = Vec::new();
        f.write_to(&mut buf).unwrap();
        let got = Frame::read_from(&mut &buf[..]).unwrap();
        assert_eq!(f, got);
    }

    #[test]
    fn frames_roundtrip() {
        roundtrip(Frame::Write { id: 7, data: b"ls -la\n".to_vec() });
        roundtrip(Frame::Output { id: 0, data: vec![0x1b, b'[', b'2', b'J'] });
        roundtrip(Frame::Output { id: 42, data: Vec::new() }); // empty payload is valid
    }

    #[test]
    fn control_roundtrips_via_frame() {
        let mut buf = Vec::new();
        Frame::Control(Msg::Create { cwd: "/tmp".into(), rows: 24, cols: 80 })
            .write_to(&mut buf)
            .unwrap();
        match Frame::read_from(&mut &buf[..]).unwrap() {
            Frame::Control(Msg::Create { cwd, rows, cols }) => {
                assert_eq!((cwd.as_str(), rows, cols), ("/tmp", 24, 80));
            }
            other => panic!("wrong frame: {other:?}"),
        }
    }

    #[test]
    fn multiple_frames_stream_in_order() {
        let mut buf = Vec::new();
        Frame::Output { id: 1, data: b"a".to_vec() }.write_to(&mut buf).unwrap();
        Frame::Output { id: 1, data: b"bc".to_vec() }.write_to(&mut buf).unwrap();
        let mut cur = &buf[..];
        assert_eq!(Frame::read_from(&mut cur).unwrap(), Frame::Output { id: 1, data: b"a".to_vec() });
        assert_eq!(Frame::read_from(&mut cur).unwrap(), Frame::Output { id: 1, data: b"bc".to_vec() });
    }
}
