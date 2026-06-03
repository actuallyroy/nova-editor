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
    cwd: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    backlog: VecDeque<u8>,
}

enum Event {
    NewClient(TcpStream),
    // `gen` tags the connection: a relaunched GUI replaces the client, and the old
    // dead socket's reader fires a late `ClientGone` — without the tag it would clear
    // the *new* connection, so Welcome never goes out and the GUI spawns a duplicate
    // daemon. Stale-gen events are ignored.
    Client(u64, Frame),
    ClientGone(u64),
    TermOut { id: TermId, data: Vec<u8> },
    TermExit { id: TermId },
}

/// Run the daemon event loop. Blocks until the last terminal exits with no client.
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
    let mut client: Option<TcpStream> = None; // write half of the current connection
    let mut attached: HashSet<TermId> = HashSet::new();
    let mut authed = false;
    let mut ever_had_term = false;
    let mut gen: u64 = 0; // current connection generation

    for ev in rx {
        match ev {
            Event::NewClient(stream) => {
                // One client at a time: a relaunched GUI replaces the old connection.
                gen += 1;
                let g = gen;
                client = stream.try_clone().ok();
                authed = false;
                attached.clear();
                if let Ok(mut rd) = stream.try_clone() {
                    let tx = tx.clone();
                    std::thread::spawn(move || loop {
                        match Frame::read_from(&mut rd) {
                            Ok(f) => {
                                if tx.send(Event::Client(g, f)).is_err() {
                                    break;
                                }
                            }
                            Err(_) => {
                                let _ = tx.send(Event::ClientGone(g));
                                break;
                            }
                        }
                    });
                }
            }
            Event::ClientGone(g) => {
                if g != gen {
                    continue; // a previous connection's late disconnect — ignore
                }
                client = None;
                attached.clear();
                if ever_had_term && terms.is_empty() {
                    break; // nothing left to keep alive
                }
            }
            Event::TermOut { id, data } => {
                if let Some(t) = terms.get_mut(&id) {
                    t.backlog.extend(data.iter().copied());
                    let over = t.backlog.len().saturating_sub(BACKLOG_CAP);
                    if over > 0 {
                        t.backlog.drain(0..over);
                    }
                }
                if attached.contains(&id) {
                    send(&mut client, Frame::Output { id, data });
                }
            }
            Event::TermExit { id } => {
                terms.remove(&id);
                attached.remove(&id);
                send(&mut client, Frame::Control(Msg::Exited { id }));
                if terms.is_empty() && client.is_none() {
                    break;
                }
            }
            Event::Client(g, _) if g != gen => {} // frame from a replaced connection
            Event::Client(_, frame) => match frame {
                Frame::Control(Msg::Hello { token: t }) => {
                    if t == token {
                        authed = true;
                        let terminals: Vec<TermInfo> = terms
                            .iter()
                            .map(|(id, t)| TermInfo { id: *id, title: t.title.clone(), cwd: t.cwd.clone() })
                            .collect();
                        send(&mut client, Frame::Control(Msg::Welcome { terminals }));
                    } else {
                        client = None; // reject unauthenticated peer
                    }
                }
                _ if !authed => {} // ignore everything until Hello succeeds
                Frame::Control(Msg::Create { cwd, rows, cols }) => {
                    if let Some(term) = spawn_term(&cwd, rows, cols, next_id, tx.clone()) {
                        let id = next_id;
                        next_id += 1;
                        let title = term.title.clone();
                        terms.insert(id, term);
                        attached.insert(id);
                        ever_had_term = true;
                        send(&mut client, Frame::Control(Msg::Created { id, title }));
                    }
                }
                Frame::Control(Msg::Attach { id }) => {
                    attached.insert(id);
                    let data = terms.get(&id).map(|t| t.backlog.iter().copied().collect()).unwrap_or_default();
                    send(&mut client, Frame::Control(Msg::Backlog { id, data }));
                }
                Frame::Write { id, data } => {
                    if let Some(t) = terms.get_mut(&id) {
                        let _ = t.writer.write_all(&data);
                        let _ = t.writer.flush();
                    }
                }
                Frame::Control(Msg::Resize { id, rows, cols }) => {
                    if let Some(t) = terms.get_mut(&id) {
                        let _ = t.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                    }
                }
                Frame::Control(Msg::Close { id }) => {
                    if let Some(mut t) = terms.remove(&id) {
                        let _ = t.child.kill();
                    }
                    attached.remove(&id);
                }
                Frame::Control(Msg::Ping) => send(&mut client, Frame::Control(Msg::Pong)),
                _ => {}
            },
        }
    }
    // Best-effort: clear the stale discovery file so the next GUI spawns a fresh one.
    if let Some(p) = super::info_path() {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

fn send(client: &mut Option<TcpStream>, frame: Frame) {
    if let Some(s) = client {
        if frame.write_to(s).is_err() {
            *client = None;
        }
    }
}

/// Spawn the platform shell on a fresh PTY plus a reader thread that pumps its
/// output into the event loop. Returns the owned `Term` on success.
fn spawn_term(cwd: &str, rows: u16, cols: u16, id: TermId, tx: Sender<Event>) -> Option<Term> {
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
        cwd: cwd.to_string(),
        master: pair.master,
        writer,
        child,
        backlog: VecDeque::new(),
    })
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
