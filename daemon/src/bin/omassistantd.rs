#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
//! `omassistantd` — run the capability broker in the foreground.
//!
//! ```text
//! omassistantd [--socket <path>] [--state-dir <path>]
//! ```
//!
//! It binds `$XDG_RUNTIME_DIR/omassistantd.sock` (0600), keeps its grants and audit log under
//! `$XDG_STATE_HOME/omassistant` (0700), and refuses to start twice. `Ctrl-C` stops it; the socket
//! file is removed on the way out, and a leftover one from a crash is replaced on the next start.

use omassistantd::VERSION;
use omassistantd::broker::{Broker, BrokerConfig};
use omassistantd::capture::ToolSet;
use omassistantd::cli::{ScanSpec, scan};
use omassistantd::clock::{self, SystemClock};
use omassistantd::exec::SystemRunner;
use omassistantd::paths::Paths;
use omassistantd::server;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "\
usage: omassistantd [--socket <path>] [--state-dir <path>]

  --socket <path>     the Unix socket to listen on (default $XDG_RUNTIME_DIR/omassistantd.sock)
  --state-dir <path>  where grants, the audit log and the shots live
                      (default $XDG_STATE_HOME/omassistant or ~/.local/state/omassistant)
  --version           print the version and exit
  --help              print this help and exit

The daemon runs in the foreground. It is one process for the whole desktop: capabilities over a
Unix socket, consent in grants.json, one audit line per call under audit/.";

const SCAN: ScanSpec = ScanSpec {
    boolean: &["help", "version"],
    valued: &["socket", "state-dir"],
};

/// Why the daemon could not run.
enum Failure {
    /// The command line was wrong (exit code 2).
    Usage(String),
    /// Something else went wrong (exit code 1).
    Failed(String),
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("omassistantd: {message}");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Failed(message)) => {
            eprintln!("omassistantd: {message}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), Failure> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let scanned = scan(&argv, &SCAN).map_err(Failure::Usage)?;
    if scanned.has("help") {
        println!("{USAGE}");
        return Ok(());
    }
    if scanned.has("version") {
        println!("omassistantd {VERSION}");
        return Ok(());
    }
    if let Some(extra) = scanned.positional.first() {
        return Err(Failure::Usage(format!(
            "unexpected argument \"{extra}\"; omassistantd takes flags only"
        )));
    }

    let paths = Paths::resolve(
        scanned.value("socket").map(PathBuf::from),
        scanned.value("state-dir").map(PathBuf::from),
    )
    .map_err(|err| Failure::Failed(err.message().to_string()))?;

    // Resolved before any thread exists: `time` refuses to guess a local offset in a threaded
    // process, and the audit file names follow that offset.
    let (_, local_offset_known) = clock::local_offset();
    if !local_offset_known {
        eprintln!(
            "omassistantd: the local UTC offset could not be determined; audit files will be \
             named by UTC day"
        );
    }

    paths
        .ensure_state()
        .map_err(|err| Failure::Failed(err.message().to_string()))?;

    let broker = Broker::new(BrokerConfig {
        paths: paths.clone(),
        clock: Arc::new(SystemClock::new()),
        runner: Arc::new(SystemRunner::new()),
        tools: ToolSet::detect(),
    })
    .map_err(|err| Failure::Failed(err.message().to_string()))?;

    let (listener, socket_guard) =
        server::bind(&paths).map_err(|err| Failure::Failed(err.message().to_string()))?;

    eprintln!(
        "omassistantd {VERSION} pid {} listening on {} (state {})",
        broker.pid(),
        paths.socket().display(),
        paths.state_dir().display()
    );

    let broker = Arc::new(broker);
    let served =
        server::serve(listener, broker).map_err(|err| Failure::Failed(err.message().to_string()));
    drop(socket_guard);
    served
}
