//! The Unix socket: bind it privately, refuse a second daemon, serve one thread per connection.
//!
//! The socket lives in `$XDG_RUNTIME_DIR` (0700 by the session manager) and is chmodded 0600, so
//! only the person can talk to it. A `XDG_RUNTIME_DIR` that does not exist yet is created 0700.
//!
//! Refusing a second instance is done by *connecting*: if something answers, there is a daemon and
//! this one exits with a plain message. If the connect fails, whatever sits at that path is a stale
//! socket from a crashed run and is removed — but only if it really is a socket.

use crate::broker::Broker;
use crate::error::{ApiError, ErrorCode};
use crate::paths::{self, FILE_MODE, Paths};
use crate::protocol::{self, Frame, MAX_REQUEST_BYTES, Response};
use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// A connection that sends nothing for this long is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// A connection that cannot be written to within this long is closed.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Removes the socket file when the daemon stops.
#[derive(Debug)]
pub struct SocketGuard {
    path: PathBuf,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Bind the socket, refusing to start a second daemon.
pub fn bind(paths: &Paths) -> Result<(UnixListener, SocketGuard), ApiError> {
    let path = paths.socket().to_path_buf();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            paths::ensure_dir(parent)?;
        }
    }
    if fs::symlink_metadata(&path).is_ok() {
        if UnixStream::connect(&path).is_ok() {
            return Err(ApiError::new(
                ErrorCode::AlreadyRunning,
                format!(
                    "omassistantd is already running on {} (refusing to start a second one)",
                    path.display()
                ),
            ));
        }
        let metadata = fs::symlink_metadata(&path).map_err(|err| {
            ApiError::new(
                ErrorCode::Unavailable,
                format!("could not inspect {}: {err}", path.display()),
            )
        })?;
        if !metadata.file_type().is_socket() {
            return Err(ApiError::new(
                ErrorCode::Unavailable,
                format!(
                    "{} exists and is not a socket; remove it or pass another --socket",
                    path.display()
                ),
            ));
        }
        fs::remove_file(&path).map_err(|err| {
            ApiError::new(
                ErrorCode::Unavailable,
                format!(
                    "could not remove the stale socket {}: {err}",
                    path.display()
                ),
            )
        })?;
    }
    let listener = UnixListener::bind(&path).map_err(|err| {
        ApiError::new(
            ErrorCode::Unavailable,
            format!("could not bind {}: {err}", path.display()),
        )
    })?;
    if let Err(err) = paths::set_mode(&path, FILE_MODE) {
        let _ = fs::remove_file(&path);
        return Err(err);
    }
    Ok((listener, SocketGuard { path }))
}

/// Accept connections until the listener fails; each connection gets its own thread.
pub fn serve(listener: UnixListener, broker: Arc<Broker>) -> Result<(), ApiError> {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let broker = Arc::clone(&broker);
                thread::spawn(move || handle_connection(stream, &broker));
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(ApiError::new(
                    ErrorCode::Unavailable,
                    format!("stopped accepting connections: {err}"),
                ));
            }
        }
    }
    Ok(())
}

/// Read requests and write responses until the peer goes away.
pub fn handle_connection(stream: UnixStream, broker: &Broker) {
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        match protocol::read_frame(&mut reader, MAX_REQUEST_BYTES) {
            Ok(Frame::Line(line)) => {
                let response = broker.handle_line(&line);
                if write_line(&mut writer, &response).is_err() {
                    return;
                }
            }
            Ok(Frame::Oversized) => {
                let error = ApiError::new(
                    ErrorCode::RequestTooLarge,
                    format!(
                        "a request may not be longer than {} KiB",
                        MAX_REQUEST_BYTES / 1024
                    ),
                );
                if write_line(&mut writer, &Response::err(None, &error).to_line()).is_err() {
                    return;
                }
            }
            Ok(Frame::Eof) => return,
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                return;
            }
            Err(_) => return,
        }
    }
}

fn write_line(writer: &mut UnixStream, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::broker::{Broker, BrokerConfig};
    use crate::capture::ToolSet;
    use crate::clock::FixedClock;
    use crate::exec::{CommandOutput, CommandRunner};
    use crate::server;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct NoTools;

    impl CommandRunner for NoTools {
        fn run(
            &self,
            program: &str,
            _args: &[&str],
            _timeout: Duration,
        ) -> Result<CommandOutput, ApiError> {
            Err(ApiError::new(
                ErrorCode::ToolMissing,
                format!("{program} must not run in this test"),
            ))
        }

        fn run_with_input(
            &self,
            program: &str,
            _args: &[&str],
            _input: &[u8],
            _timeout: Duration,
        ) -> Result<CommandOutput, ApiError> {
            Err(ApiError::new(
                ErrorCode::ToolMissing,
                format!("{program} must not run in this test"),
            ))
        }

        fn spawn_detached(&self, program: &str, _args: &[&str]) -> Result<(), ApiError> {
            Err(ApiError::new(
                ErrorCode::ToolMissing,
                format!("{program} must not run in this test"),
            ))
        }
    }

    fn temp_paths(tag: &str) -> Paths {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "omassistantd-server-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        Paths::new(dir.join("run").join("omassistantd.sock"), dir.join("state"))
    }

    fn broker(paths: Paths) -> Broker {
        Broker::new(BrokerConfig {
            paths,
            clock: Arc::new(FixedClock::from_unix(1_700_000_000).unwrap()),
            runner: Arc::new(NoTools),
            tools: ToolSet::none(),
        })
        .unwrap()
    }

    #[test]
    fn the_socket_is_private_and_only_one_daemon_can_have_it() {
        let paths = temp_paths("bind");
        let (listener, guard) = bind(&paths).unwrap();
        let mode = fs::metadata(paths.socket()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE, "the socket must be 0600");
        assert_eq!(
            fs::metadata(paths.socket().parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the directory we created must be 0700"
        );

        let err = bind(&paths).unwrap_err();
        assert_eq!(err.code(), ErrorCode::AlreadyRunning);
        assert!(
            err.message().contains("already running"),
            "{}",
            err.message()
        );

        drop(listener);
        drop(guard);
        assert!(!paths.socket().exists(), "the socket is removed on drop");
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_stale_socket_is_replaced_but_a_real_file_is_not() {
        let paths = temp_paths("stale");
        std::fs::create_dir_all(paths.socket().parent().unwrap()).unwrap();
        // Dropping a listener leaves its socket file behind — a crashed run's leftover.
        drop(UnixListener::bind(paths.socket()).unwrap());
        assert!(paths.socket().exists());
        let (listener, guard) = bind(&paths).unwrap();
        drop(listener);
        drop(guard);

        // The guard removed the socket on the way out; put a plain file in its place.
        fs::write(paths.socket(), b"a regular file").unwrap();
        let err = bind(&paths).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(err.message().contains("not a socket"), "{}", err.message());
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_connection_gets_one_line_per_request() {
        let paths = temp_paths("serve");
        let (listener, _guard) = bind(&paths).unwrap();
        let broker = Arc::new(broker(paths.clone()));
        let thread = std::thread::spawn(move || server::serve(listener, broker));

        let stream = UnixStream::connect(paths.socket()).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        writer
            .write_all(b"{\"id\":1,\"method\":\"ping\"}\n")
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        let response = crate::protocol::Response::from_line(line.trim()).unwrap();
        assert!(response.ok);
        assert_eq!(response.id, Some(serde_json::json!(1)));

        // An oversized line is refused, and the connection stays usable.
        let big = "x".repeat(MAX_REQUEST_BYTES + 1);
        writer.write_all(big.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        let response = crate::protocol::Response::from_line(line.trim()).unwrap();
        assert!(!response.ok);
        assert_eq!(
            response.error.unwrap().code,
            "request_too_large".to_string()
        );
        writer.write_all(b"{\"method\":\"ping\"}\n").unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        assert!(
            crate::protocol::Response::from_line(line.trim())
                .unwrap()
                .ok
        );

        drop(writer);
        drop(thread);
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }
}
