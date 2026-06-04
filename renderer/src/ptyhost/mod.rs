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
    /// Set a terminal's display title (tab rename) — stored in the daemon so the
    /// name survives a GUI restart / re-attach.
    Rename { id: TermId, title: String },
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
    /// Is one specific terminal's shell running a foreground process? (Drives the
    /// terminal's smart-Escape: a busy shell gets the real ESC, an idle prompt
    /// clears the input line.) Replies `TermBusyResult`.
    QueryTermBusy { id: TermId },
    TermBusyResult { id: TermId, busy: bool },
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
    /// Current pty dimensions — the re-attach grid must match them while the
    /// backlog replays, or cursor-addressed TUI frames land on wrong rows (#32).
    /// `default` so a TermInfo from an older daemon still parses (0 ⇒ unknown,
    /// the client falls back to its panel size).
    #[serde(default)]
    pub rows: u16,
    #[serde(default)]
    pub cols: u16,
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
    /// A frame from a NEWER peer this build can't decode — consumed and skipped
    /// (never re-sent). Read loops must treat it as a no-op, not an error.
    Ignored,
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
            Frame::Ignored => return Ok(()), // placeholder for skipped input; never sent
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
                // Forward compatibility: a control message this build doesn't know
                // (a newer peer's addition) must NOT kill the connection — that
                // would detach every terminal over one optional feature. The frame
                // is already consumed off the stream, so skipping it is safe; the
                // sender simply doesn't get that feature from an older peer. This
                // is what lets app updates keep their running sessions.
                match serde_json::from_slice(payload) {
                    Ok(msg) => Ok(Frame::Control(msg)),
                    Err(_) => Ok(Frame::Ignored),
                }
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
            // Unknown frame tags are length-delimited too — skip, don't disconnect.
            _ => Ok(Frame::Ignored),
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
/// Wire-protocol generation. Bump ONLY for breaking changes — additive messages
/// and fields don't need one anymore (unknown control frames are skipped and new
/// TermInfo fields default), so app updates keep their running sessions.
/// History: v4 TermInfo rows/cols; v3 Msg::Rename; v2 multi-client.
pub const PROTO_VERSION: u32 = 4;

/// Which daemon family this build talks to. Debug and release builds get their
/// OWN daemon (sharing one made them fight: duplicate daemons, debug launches
/// forwarded to release windows). This used to hash the exe PATH, but that key
/// is unstable — macOS app translocation runs the app from a randomized path,
/// and DefaultHasher isn't pinned across toolchains — which minted a fresh
/// daemon (and stranded the old one + its shells) on updates and even launches.
fn flavor() -> &'static str {
    if cfg!(debug_assertions) { "dev" } else { "app" }
}

fn info_name(flavor: &str) -> String {
    format!("ptyhost-v{PROTO_VERSION}-{flavor}.json")
}

pub fn info_path() -> Option<std::path::PathBuf> {
    crate::settings::config_dir().map(|d| d.join(info_name(flavor())))
}

/// Shut down daemons stranded by past protocol bumps or the old exe-path-hash
/// naming. Without this sweep an in-app update leaks the previous daemon and its
/// shells in the background forever — running, invisible, undiscoverable.
/// Closing a daemon drops its pty masters, so its shells receive SIGHUP and
/// exit. Everything except the two CURRENT discovery files (this build's flavor
/// and its sibling — the sibling's daemon may be live) is fair game; the pid is
/// verified to be an `aether --pty-host` before any kill.
pub fn cleanup_stale_daemons() {
    let Some(dir) = crate::settings::config_dir() else { return };
    let keep = [info_name("dev"), info_name("app")];
    let Ok(rd) = std::fs::read_dir(&dir) else { return };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("ptyhost") || !name.ends_with(".json") || keep.iter().any(|k| *k == name) {
            continue;
        }
        // Old-format files at the CURRENT protocol version may belong to a live
        // daemon that a not-yet-updated install is actively using — only reap
        // those once their daemon has exited (dead pid ⇒ remove the file).
        // Anything below the current version is unreachable by every current
        // build: kill the daemon (verified) and remove its file.
        let current_ver = name
            .strip_prefix(&format!("ptyhost-v{PROTO_VERSION}-"))
            .map_or(false, |_| true);
        let info = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|t| serde_json::from_str::<HostInfo>(&t).ok());
        match info {
            Some(info) if current_ver => {
                if process_alive(info.pid) {
                    // Same binary as us ⇒ our own install's pre-rename daemon:
                    // migrate it (kill; its file format is unreachable to us).
                    // A DIFFERENT binary's live daemon is another install mid-
                    // update — leave it for that install's own sweep.
                    if daemon_cmdline(info.pid).map_or(true, |c| !c.contains(&current_exe_str())) {
                        continue;
                    }
                    kill_if_ptyhost(info.pid);
                }
            }
            Some(info) => kill_if_ptyhost(info.pid),
            None => {}
        }
        let _ = std::fs::remove_file(entry.path());
    }
}

fn current_exe_str() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The command line of `pid`, if readable.
fn daemon_cmdline(pid: u32) -> Option<String> {
    #[cfg(unix)]
    {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("wmic")
            .args(["process", "where", &format!("ProcessId={pid}"), "get", "CommandLine"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
    }
}

/// Liveness probe (any process). Shell-based so it needs no extra crates; this
/// runs once per stale file at startup, not on a hot path.
fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map_or(false, |s| s.success())
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map_or(false, |o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
    }
}

/// Kill `pid` only if it is verifiably an `aether --pty-host` process (PID reuse
/// must never take down an unrelated program).
fn kill_if_ptyhost(pid: u32) {
    #[cfg(unix)]
    {
        let cmdline = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if cmdline.contains("--pty-host") {
            let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
        }
    }
    #[cfg(windows)]
    {
        let list = std::process::Command::new("wmic")
            .args(["process", "where", &format!("ProcessId={pid}"), "get", "CommandLine"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        if list.contains("--pty-host") {
            let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
        }
    }
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
