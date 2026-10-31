//! Single instance, plus a local control channel.
//!
//! The first instance holds a lock file and listens on a loopback port. A
//! second launch finds the lock taken, asks the running instance to show its
//! panel, and exits — the same as Electron's `requestSingleInstanceLock`.
//!
//! With `VEILPILOT_DEV=1` the channel also accepts commands that drive the UI
//! (actions, typing, clicks, snapshots). That is how the end-to-end tests in
//! `scripts/e2e.sh` exercise the real app. Without the variable, only `show`
//! is accepted.

use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use futures::channel::{mpsc, oneshot};

use crate::config::data_dir;

pub struct Command {
    pub line: String,
    pub reply: oneshot::Sender<String>,
}

pub enum Launch {
    /// We are the instance. Keep the lock alive for the life of the app.
    Primary { _lock: File, commands: mpsc::UnboundedReceiver<Command> },
    /// Another instance is running and has been asked to show itself.
    Secondary,
}

fn port_path() -> std::path::PathBuf {
    data_dir().join("instance.port")
}

pub fn dev_mode() -> bool {
    std::env::var_os("VEILPILOT_DEV").is_some_and(|v| v == "1")
}

/// Send one command to the running instance and return its reply.
pub fn send(line: &str) -> std::io::Result<String> {
    let port: u16 =
        std::fs::read_to_string(port_path())?.trim().parse().map_err(|_| std::io::Error::other("bad port file"))?;
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(120)))?;
    stream.write_all(format!("{line}\n").as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    Ok(reply.trim_end().to_string())
}

pub fn acquire() -> std::io::Result<Launch> {
    std::fs::create_dir_all(data_dir())?;
    let lock = File::options().create(true).truncate(false).write(true).open(data_dir().join("instance.lock"))?;
    if lock.try_lock().is_err() {
        let _ = send("show");
        return Ok(Launch::Secondary);
    }

    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    std::fs::write(port_path(), listener.local_addr()?.port().to_string())?;

    let (tx, commands) = mpsc::unbounded();
    let dev = dev_mode();
    std::thread::Builder::new().name("veilpilot-control".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => return,
                });
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let line = line.trim().to_string();
                let reply = if line != "show" && !dev {
                    "error: control commands need VEILPILOT_DEV=1".to_string()
                } else {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    if tx.unbounded_send(Command { line, reply: reply_tx }).is_err() {
                        return;
                    }
                    futures::executor::block_on(reply_rx).unwrap_or_else(|_| "error: dropped".into())
                };
                let mut stream = stream;
                let _ = stream.write_all(format!("{}\n", reply.replace('\n', "\\n")).as_bytes());
            });
        }
    })?;

    Ok(Launch::Primary { _lock: lock, commands })
}
