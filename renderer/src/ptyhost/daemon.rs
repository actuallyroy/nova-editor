// Pty-host daemon: owns every terminal's PTY + shell process and a per-terminal
// output backlog, so they outlive the GUI. Single-threaded event loop owns the one
// client socket; PTY reader threads and the connection acceptor feed it events.
//
// Run as `aether --pty-host`. Discovery + auth go through `~/.aether/ptyhost.json`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{channel, Sender};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use super::{Frame, HostInfo, Msg, TermId, TermInfo};

const BACKLOG_CAP: usize = 1 << 20; // keep ~1 MiB of recent VT bytes per terminal

struct Term {
    title: String,
    workspace: String,
    /// The connection that currently holds this terminal; `None` = orphaned (its
    /// window closed). Output is routed to the owner; an orphan can be re-claimed by a
    /// new connection in the same workspace. This is what isolates windows from each
    /// other while still letting a restart reclaim its own shells.
    owner: Option<u64>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    backlog: VecDeque<u8>,
    /// Current pty dimensions. Re-attach replays the backlog into a grid of THIS
    /// size (the bytes were emitted for it); the GUI then resizes to its panel,
    /// and the SIGWINCH makes TUIs (Claude Code) repaint cleanly (#32).
    rows: u16,
    cols: u16,
}

enum Event {
    NewClient(TcpStream),
    Client(u64, Frame), // (connection id, frame)
    ClientGone(u64),
    TermOut { id: TermId, data: Vec<u8> },
    TermExit { id: TermId },
}

/// Run the daemon event loop. One daemon per machine serves every window; clients
/// are isolated by terminal ownership + workspace. Exits when no terminals and no
/// connections remain.
pub fn run() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let token = gen_token(port);
    write_info(port, &token)?;

    let (tx, rx) = channel::<Event>();

    // Acceptor thread → NewClient events.
    {
        let tx = tx.clone();
        let listener = listener.try_clone()?;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_nodelay(true);
                if tx.send(Event::NewClient(stream)).is_err() {
                    break;
                }
            }
        });
    }

    let mut terms: HashMap<TermId, Term> = HashMap::new();
    let mut next_id: TermId = 1;
    let mut next_conn: u64 = 1;
    let mut conns: HashMap<u64, TcpStream> = HashMap::new(); // conn id → write half
    let mut authed: HashSet<u64> = HashSet::new();
    let mut workspaces: HashMap<u64, String> = HashMap::new(); // conn id → workspace root
    let mut ever_connected = false;

    for ev in rx {
        match ev {
            Event::NewClient(stream) => {
                let cid = next_conn;
                next_conn += 1;
                ever_connected = true;
                if let Ok(w) = stream.try_clone() {
                    conns.insert(cid, w);
                }
                if let Ok(mut rd) = stream.try_clone() {
                    let tx = tx.clone();
                    std::thread::spawn(move || loop {
                        match Frame::read_from(&mut rd) {
                            Ok(f) => {
                                if tx.send(Event::Client(cid, f)).is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                let _ = tx.send(Event::ClientGone(cid));
                                break;
                            }
                        }
                    });
                }
            }
            Event::ClientGone(cid) => {
                conns.remove(&cid);
                authed.remove(&cid);
                workspaces.remove(&cid);
                // Orphan this window's terminals (keep them running for re-claim).
                for t in terms.values_mut() {
                    if t.owner == Some(cid) {
                        t.owner = None;
                    }
                }
                if ever_connected && terms.is_empty() && conns.is_empty() {
                    break; // nothing left to serve
                }
            }
            Event::TermOut { id, data } => {
                let owner = if let Some(t) = terms.get_mut(&id) {
                    t.backlog.extend(data.iter().copied());
                    let over = t.backlog.len().saturating_sub(BACKLOG_CAP);
                    if over > 0 {
                        t.backlog.drain(0..over);
                    }
                    t.owner
                } else {
                    None
                };
                if let Some(cid) = owner {
                    send(&mut conns, cid, Frame::Output { id, data });
                }
            }
            Event::TermExit { id } => {
                let owner = terms.remove(&id).and_then(|t| t.owner);
                if let Some(cid) = owner {
                    send(&mut conns, cid, Frame::Control(Msg::Exited { id }));
                }
                if terms.is_empty() && conns.is_empty() {
                    break;
                }
            }
            Event::Client(cid, frame) => {
                // Handshake first; everything else requires an authed connection.
                if let Frame::Control(Msg::Hello { token: t, workspace }) = &frame {
                    if *t == token {
                        authed.insert(cid);
                        workspaces.insert(cid, workspace.clone());
                        // Offer only orphaned terminals from this workspace.
                        let terminals: Vec<TermInfo> = terms
                            .iter()
                            .filter(|(_, t)| t.owner.is_none() && &t.workspace == workspace)
                            .map(|(id, t)| TermInfo { id: *id, title: t.title.clone(), cwd: t.workspace.clone(), rows: t.rows, cols: t.cols })
                            .collect();
                        send(&mut conns, cid, Frame::Control(Msg::Welcome { terminals }));
                    } else {
                        conns.remove(&cid); // reject
                    }
                    continue;
                }
                if !authed.contains(&cid) {
                    continue;
                }
                match frame {
                    Frame::Control(Msg::Create { cwd, rows, cols }) => {
                        let workspace = workspaces.get(&cid).cloned().unwrap_or_else(|| cwd.clone());
                        if let Some(term) = spawn_term(&cwd, &workspace, cid, rows, cols, next_id, tx.clone()) {
                            let id = next_id;
                            next_id += 1;
                            let title = term.title.clone();
                            terms.insert(id, term);
                            send(&mut conns, cid, Frame::Control(Msg::Created { id, title }));
                        }
                    }
                    Frame::Control(Msg::Attach { id }) => {
                        // Claim an orphaned (or already-owned) terminal; refuse one a
                        // live window still holds.
                        if let Some(t) = terms.get_mut(&id) {
                            if t.owner.is_none() || t.owner == Some(cid) {
                                t.owner = Some(cid);
                                let data: Vec<u8> = t.backlog.iter().copied().collect();
                                send(&mut conns, cid, Frame::Control(Msg::Backlog { id, data }));
                            }
                        }
                    }
                    Frame::Write { id, data } => {
                        if let Some(t) = terms.get_mut(&id) {
                            if t.owner == Some(cid) {
                                let _ = t.writer.write_all(&data);
                                let _ = t.writer.flush();
                            }
                        }
                    }
                    Frame::Control(Msg::Resize { id, rows, cols }) => {
                        if let Some(t) = terms.get_mut(&id) {
                            if t.owner == Some(cid) {
                                let _ = t.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                                t.rows = rows;
                                t.cols = cols;
                            }
                        }
                    }
                    Frame::Control(Msg::Close { id }) => {
                        if terms.get(&id).map(|t| t.owner == Some(cid)).unwrap_or(false) {
                            if let Some(mut t) = terms.remove(&id) {
                                let _ = t.child.kill();
                            }
                        }
                    }
                    Frame::Control(Msg::Rename { id, title }) => {
                        // Tab rename: stored here so re-attach (Welcome/TermInfo)
                        // restores the custom name after a GUI restart.
                        if let Some(t) = terms.get_mut(&id) {
                            if t.owner == Some(cid) {
                                t.title = title;
                            }
                        }
                    }
                    Frame::Control(Msg::Detach { id }) => {
                        if let Some(t) = terms.get_mut(&id) {
                            if t.owner == Some(cid) {
                                t.owner = None; // orphan: keeps running, reclaimable later
                            }
                        }
                    }
                    Frame::Control(Msg::QueryBusy) => {
                        let count = terms.values().filter(|t| t.owner == Some(cid) && shell_busy(t)).count();
                        send(&mut conns, cid, Frame::Control(Msg::BusyResult { count }));
                    }
                    Frame::Control(Msg::QueryTermBusy { id }) => {
                        let busy = terms.get(&id).map_or(false, shell_busy);
                        send(&mut conns, cid, Frame::Control(Msg::TermBusyResult { id, busy }));
                    }
                    Frame::Control(Msg::FocusWindow { workspace }) => {
                        // Single-window-per-folder: find another live window that has
                        // this workspace open and ask it to raise itself. Empty
                        // workspaces (folder-less windows) never match each other.
                        let target = if workspace.is_empty() {
                            None
                        } else {
                            workspaces
                                .iter()
                                .find(|(other, w)| **other != cid && *w == &workspace)
                                .map(|(other, _)| *other)
                        };
                        if let Some(other) = target {
                            send(&mut conns, other, Frame::Control(Msg::Focus));
                        }
                        send(&mut conns, cid, Frame::Control(Msg::FocusResult { found: target.is_some() }));
                    }
                    Frame::Control(Msg::SetWorkspace { workspace }) => {
                        // Re-offer the new folder's orphaned shells (a folder-less
                        // window that just opened a project can now restore them).
                        let terminals: Vec<TermInfo> = terms
                            .iter()
                            .filter(|(_, t)| t.owner.is_none() && !workspace.is_empty() && t.workspace == workspace)
                            .map(|(id, t)| TermInfo { id: *id, title: t.title.clone(), cwd: t.workspace.clone(), rows: t.rows, cols: t.cols })
                            .collect();
                        workspaces.insert(cid, workspace);
                        send(&mut conns, cid, Frame::Control(Msg::Welcome { terminals }));
                    }
                    Frame::Control(Msg::Ping) => send(&mut conns, cid, Frame::Control(Msg::Pong)),
                    _ => {}
                }
            }
        }
    }
    // Best-effort: clear the stale discovery file so the next GUI spawns a fresh one.
    if let Some(p) = super::info_path() {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

fn send(conns: &mut HashMap<u64, TcpStream>, cid: u64, frame: Frame) {
    if let Some(s) = conns.get_mut(&cid) {
        if frame.write_to(s).is_err() {
            conns.remove(&cid);
        }
    }
}

/// Spawn the platform shell on a fresh PTY plus a reader thread that pumps its
/// output into the event loop. Returns the owned `Term` on success.
fn spawn_term(cwd: &str, workspace: &str, owner: u64, rows: u16, cols: u16, id: TermId, tx: Sender<Event>) -> Option<Term> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .ok()?;
    let shell = platform_shell();
    let title = std::path::Path::new(&shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("shell")
        .to_string();
    let mut cmd = CommandBuilder::new(&shell);
    let cwdp = std::path::Path::new(cwd);
    if cwdp.is_dir() {
        cmd.cwd(cwdp);
    }
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let child = pair.slave.spawn_command(cmd).ok()?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().ok()?;
    let writer = pair.master.take_writer().ok()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Event::TermExit { id });
                    break;
                }
                Ok(n) => {
                    if tx.send(Event::TermOut { id, data: buf[..n].to_vec() }).is_err() {
                        break;
                    }
                }
            }
        }
    });
    Some(Term {
        title,
        workspace: workspace.to_string(),
        owner: Some(owner),
        master: pair.master,
        writer,
        child,
        backlog: VecDeque::new(),
        rows,
        cols,
    })
}

/// True when the terminal's shell has a child process (a running command/TUI, not
/// just an idle prompt). Unix: any child of the shell pid. Windows: not detected
/// yet — reports idle, so closing never warns there.
fn shell_busy(t: &Term) -> bool {
    #[cfg(unix)]
    {
        if let Some(pid) = t.child.process_id() {
            return std::process::Command::new("pgrep")
                .arg("-P")
                .arg(pid.to_string())
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
        }
        false
    }
    #[cfg(windows)]
    {
        let _ = t;
        false
    }
}

/// The user's login shell (COMSPEC on Windows, else $SHELL → bash → sh).
pub fn platform_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| {
            if std::path::Path::new("/bin/bash").exists() {
                "/bin/bash".to_string()
            } else {
                "/bin/sh".to_string()
            }
        })
    }
}

/// A non-cryptographic token; the real gate is the 0600 discovery file. Enough to
/// stop a stray process from attaching to a port it never read the file for.
fn gen_token(port: u16) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut h);
    std::process::id().hash(&mut h);
    port.hash(&mut h);
    let a = h.finish();
    a.hash(&mut h);
    let b = h.finish();
    format!("{a:016x}{b:016x}")
}

fn write_info(port: u16, token: &str) -> io::Result<()> {
    let Some(path) = super::info_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let info = HostInfo { port, token: token.to_string(), pid: std::process::id() };
    let json = serde_json::to_vec_pretty(&info).map_err(io::Error::other)?;
    std::fs::write(&path, json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
