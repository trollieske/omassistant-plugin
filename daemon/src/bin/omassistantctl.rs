#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
//! `omassistantctl` — the debug, acting and consent CLI for `omassistantd`.
//!
//! ```text
//! omassistantctl ping
//! omassistantctl capabilities
//! omassistantctl observe.screen [--target window|screen] [--json]
//! omassistantctl act.type --text <text>        (or --text - for stdin)
//! omassistantctl act.key <chord>               (e.g. super+return)
//! omassistantctl act.clipboard read|write [--text <text> | -]
//! omassistantctl consent grant observe.screen --once | --for 15m
//! omassistantctl consent revoke observe.screen
//! omassistantctl consent revoke-all
//! omassistantctl consent list
//! omassistantctl audit [--lines N]
//! omassistantctl costs [--json]        total and per-conversation model cost
//! ```
//!
//! Human output by default, `--json` for the machine-readable result. Exit codes: `0` fine, `1` the
//! call failed (the plain message goes to stderr), `2` the command line was wrong.

use omassistantd::VERSION;
use omassistantd::audit::MAX_TAIL_LINES;
use omassistantd::cli::{ScanSpec, Scanned, human_bytes, parse_duration, scan};
use omassistantd::client::{Client, RemoteError};
use omassistantd::costs::{CostReport, scan as scan_costs};
use omassistantd::grants::{MAX_TTL_SECONDS, MIN_TTL_SECONDS};
use omassistantd::paths::Paths;
use omassistantd::protocol::MAX_REQUEST_BYTES;
use serde_json::{Value, json};
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: omassistantctl [--socket <path>] [--state-dir <path>] [--json] <command>

  ping                                  is the daemon there, and which version is it
  capabilities                          what the daemon can do, and what is currently granted
  observe.screen [--target window|screen]
                                        ask for one capture; prints where it went
  act.type --text <text> | --text -     type text into the focused window (- reads stdin)
  act.key <chord>                       press one chord, e.g. super+return, ctrl+shift+t, escape
  act.clipboard read                    print what is on the clipboard
  act.clipboard write --text <text>|-   put text on the clipboard (- reads stdin)
  act.launch browser|terminal|editor|mail  open the app you have for that kind
  act.launch --app <id>                 open any installed app by its desktop id
  app.list                              installed apps: id and display name
  shell.exec -- <command>               run one command in your shell (asks the card)
  web.search <query>                    look something up on the web (asks the card)
  web.fetch <url>                       read one public page as text
  shell.run <action>                    run one named desktop action
                                        (lock, volume-up, volume-down, volume-mute,
                                         brightness-up, brightness-down, next-workspace,
                                         previous-workspace, toggle-layout)
  consent grant <capability> --once     grant one call (default TTL 5 minutes)
  consent grant <capability> --for 1h   grant for a while (60s..24h)
  consent revoke <capability>           drop a grant
  consent list                          what is granted right now
  consent pending                       confirmations waiting for the card
  consent decide <id> --allow|--deny    answer one of them
  consent revoke-run                    drop the card's run-scoped approvals
  consent revoke-all                    every grant, and fail waiting confirmations
  audit [--lines N]                     the last audit entries, newest last (1..200)
  costs [--json]                        usage and cost from pi's session files (no daemon needed)
  web.config --provider <name>          set the web search provider (keyless, you, brave,
                                        tavily, serper); the key is read from stdin
  web.status                            which provider answers, and whether a key is set
  web.last                              the pictures the last search came with
  trust on|off|status                   stop asking on this machine (still audited)

  --socket <path>     talk to this socket instead of $XDG_RUNTIME_DIR/omassistantd.sock
  --state-dir <path>  the state directory (only used to resolve defaults)
  --json              print the raw JSON result instead of words
  --text <text>       the text to type or copy; `-` reads it from stdin, verbatim
  --version           print the version and exit
  --help              print this help and exit

Acting needs its own grant: `omassistantctl consent grant act.keys --once` to type or press a key,
`omassistantctl consent grant act.clipboard --once` for the clipboard, `act.launch` for a launch. Keys go to whichever window
has focus, so close the overlay before calling. The daemon holds the grants, the audit log and the
captures.";

const SCAN: ScanSpec = ScanSpec {
    boolean: &[
        "help", "version", "json", "once", "allow", "deny", "key-set",
    ],
    valued: &[
        "socket",
        "state-dir",
        "target",
        "lines",
        "for",
        "text",
        "app",
        "cwd",
        "timeout",
        "provider",
        "key",
    ],
};

/// Why the CLI stopped.
enum Failure {
    /// The command line was wrong (exit code 2).
    Usage(String),
    /// The call failed, or the daemon said no (exit code 1).
    Failed(String),
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("omassistantctl: {message}");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Failed(message)) => {
            eprintln!("omassistantctl: {message}");
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
        println!("omassistantctl {VERSION} (omassistantd {VERSION})");
        return Ok(());
    }

    let command = match scanned.positional.first() {
        Some(command) => command.clone(),
        None => return Err(Failure::Usage("no command given".to_string())),
    };
    let arguments: Vec<String> = scanned.positional[1..].to_vec();
    let paths = Paths::resolve(
        scanned.value("socket").map(PathBuf::from),
        scanned.value("state-dir").map(PathBuf::from),
    )
    .map_err(|err| Failure::Failed(err.message().to_string()))?;
    let json = scanned.has("json");

    if command == "trust" {
        let mode = arguments.first().map(String::as_str).unwrap_or("status");
        let path = paths.state_dir().join("trust.json");
        match mode {
            "status" => {
                let raw = std::fs::read_to_string(&path).unwrap_or_default();
                let on = serde_json::from_str::<serde_json::Value>(&raw)
                    .ok()
                    .and_then(|value| value.get("allowAll").and_then(serde_json::Value::as_bool))
                    .unwrap_or(false);
                if json {
                    print_json(&serde_json::json!({ "allowAll": on }))?;
                } else {
                    println!(
                        "{}",
                        if on {
                            "trusted: nothing asks on this machine, everything is audited"
                        } else {
                            "not trusted: every capability asks the card first"
                        }
                    );
                }
            }
            "on" | "off" => {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                std::fs::create_dir_all(paths.state_dir()).map_err(|err| {
                    Failure::Failed(format!("could not create the state directory: {err}"))
                })?;
                let on = mode == "on";
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&path)
                    .map_err(|err| Failure::Failed(format!("could not write {path:?}: {err}")))?;
                use std::io::Write;
                file.write_all(serde_json::json!({ "allowAll": on }).to_string().as_bytes())
                    .map_err(|err| Failure::Failed(format!("could not write {path:?}: {err}")))?;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|err| Failure::Failed(format!("could not chmod {path:?}: {err}")))?;
                if on {
                    println!("trusted: nothing asks on this machine. Every call is still audited.");
                } else {
                    println!("not trusted: every capability asks the card first again.");
                }
            }
            other => {
                return Err(Failure::Usage(format!(
                    "trust takes on, off or status, not \"{other}\""
                )));
            }
        }
        return Ok(());
    }

    // The search provider config is a local file; no daemon socket required.
    if command == "web.config" {
        let provider = scanned.value("provider").unwrap_or("keyless").to_string();
        let Some(provider) = omassistantd::web::Provider::parse(&provider) else {
            return Err(Failure::Usage(format!(
                "\"{provider}\" is not a search provider; use keyless, you, brave, tavily or serper"
            )));
        };
        // The key arrives on stdin so it never appears in `ps` or a shell history.
        let mut key = String::new();
        if provider != omassistantd::web::Provider::Keyless {
            if scanned.has("key-set") || scanned.has("key") {
                key = scanned.value("key").unwrap_or_default().to_string();
            } else {
                // One line: the caller writes the key and keeps its side of the
                // pipe open, so reading to EOF would wait for a close that never
                // comes.
                use std::io::BufRead;
                std::io::stdin()
                    .lock()
                    .read_line(&mut key)
                    .map_err(|err| Failure::Failed(format!("could not read the key: {err}")))?;
            }
            key = key.trim().to_string();
            if key.is_empty() {
                return Err(Failure::Usage(
                    "a key is required for that provider; pipe it on stdin".to_string(),
                ));
            }
        }
        omassistantd::web::write_config(
            paths.state_dir(),
            &omassistantd::web::Config { provider, key },
        )
        .map_err(|err| Failure::Failed(format!("could not write the config: {err}")))?;
        println!("web search: {} ({})", provider.name(), provider.host());
        return Ok(());
    }
    if command == "web.status" {
        let config = omassistantd::web::load_config(paths.state_dir());
        if json {
            print_json(&serde_json::json!({
                "provider": config.provider.name(),
                "host": config.provider.host(),
                "keySet": !config.key.is_empty(),
            }))?;
        } else {
            println!(
                "{} ({}), key {}",
                config.provider.name(),
                config.provider.host(),
                if config.key.is_empty() {
                    "not set"
                } else {
                    "set"
                }
            );
        }
        return Ok(());
    }

    // Costs are a local report from pi's own files; no daemon socket required.
    if command == "costs" {
        expect_no_arguments(&arguments)?;
        let report = scan_costs(&paths.state_dir().join("pi-sessions"))
            .map_err(|err| Failure::Failed(err.message().to_string()))?;
        if json {
            let value = serde_json::to_value(&report).map_err(|err| {
                Failure::Failed(format!("could not encode the cost report: {err}"))
            })?;
            print_json(&value)?;
        } else {
            print_costs(&report);
        }
        return Ok(());
    }

    let mut client = Client::connect(paths.socket())
        .map_err(|err| Failure::Failed(err.message().to_string()))?;

    match command.as_str() {
        "ping" => {
            expect_no_arguments(&arguments)?;
            let result = call(&mut client, "ping", None)?;
            if json {
                print_json(&result)?;
            } else {
                print_ping(&result, &paths);
            }
        }
        "capabilities" => {
            expect_no_arguments(&arguments)?;
            let result = call(&mut client, "capabilities", None)?;
            if json {
                print_json(&result)?;
            } else {
                print_capabilities(&result);
            }
        }
        "observe.screen" => {
            expect_no_arguments(&arguments)?;
            let target = match scanned.value("target") {
                None => json!("window"),
                Some(target) => json!(target),
            };
            let result = call(
                &mut client,
                "observe.screen",
                Some(&json!({ "target": target })),
            )?;
            if json {
                print_json(&result)?;
            } else {
                print_observe(&result);
            }
        }
        "act.type" => {
            expect_no_arguments(&arguments)?;
            let body = text_argument(&scanned, None)?;
            let result = call(&mut client, "act.type", Some(&json!({ "text": body })))?;
            if json {
                print_json(&result)?;
            } else {
                println!(
                    "typed {} characters into \"{}\" — \"{}\"",
                    shown(&result, "chars"),
                    text(&result["target"], "class"),
                    text(&result["target"], "title")
                );
            }
        }
        "act.key" => {
            // Either `act.key super+return` or `act.key --text super+return`.
            let chord = text_argument(&scanned, arguments.first().map(String::as_str))?;
            if arguments.len() > 1 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\" after the chord",
                    arguments[1]
                )));
            }
            let result = call(&mut client, "act.key", Some(&json!({ "key": chord })))?;
            if json {
                print_json(&result)?;
            } else {
                println!(
                    "pressed {} in \"{}\" — \"{}\"",
                    text(&result, "chord"),
                    text(&result["target"], "class"),
                    text(&result["target"], "title")
                );
            }
        }
        "act.clipboard" => act_clipboard(&mut client, &scanned, &arguments, json)?,
        "web.search" => {
            let query = arguments.join(" ");
            let mut params = json!({ "query": query });
            if let Some(text) = scanned.value("text") {
                params["query"] = json!(text);
            }
            let result = call(&mut client, "web.search", Some(&params))?;
            if json {
                print_json(&result)?;
            } else {
                let hits = result["results"].as_array().cloned().unwrap_or_default();
                if hits.is_empty() {
                    println!("nothing found");
                }
                for hit in hits {
                    println!(
                        "{}\t{}\t[{}]",
                        text(&hit, "when"),
                        text(&hit, "title"),
                        text(&hit, "source")
                    );
                    let snippet = text(&hit, "snippet");
                    if !snippet.is_empty() {
                        println!("\t{snippet}");
                    }
                    println!("\t{}", text(&hit, "url"));
                }
            }
        }
        "web.last" => {
            let result = call(&mut client, "web.last", None)?;
            if json {
                print_json(&result)?;
            } else {
                let images = result["images"].as_array().cloned().unwrap_or_default();
                if images.is_empty() {
                    println!("no pictures from the last search");
                }
                for image in images {
                    println!("{}\t{}", text(&image, "host"), text(&image, "url"));
                }
            }
        }
        "web.fetch" => {
            let url = arguments
                .first()
                .cloned()
                .ok_or_else(|| Failure::Usage("web.fetch needs a URL".to_string()))?;
            let result = call(&mut client, "web.fetch", Some(&json!({ "url": url })))?;
            if json {
                print_json(&result)?;
            } else {
                println!("{}", text(&result, "text"));
            }
        }
        "shell.run" => {
            let action = arguments.first().ok_or_else(|| {
                Failure::Usage(
                    "shell.run needs an action: lock, volume-up, volume-down, volume-mute, \
                     brightness-up, brightness-down, next-workspace, previous-workspace, \
                     toggle-layout, fullscreen, maximize or restore-window"
                        .to_string(),
                )
            })?;
            if arguments.len() > 1 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\" after the action",
                    arguments[1]
                )));
            }
            let result = call(&mut client, "shell.run", Some(&json!({ "action": action })))?;
            if json {
                print_json(&result)?;
            } else {
                println!("ran \"{}\"", text(&result, "action"));
            }
        }
        "app.list" => {
            expect_no_arguments(&arguments)?;
            let result = call(&mut client, "app.list", None)?;
            if json {
                print_json(&result)?;
            } else {
                for app in result["apps"].as_array().cloned().unwrap_or_default() {
                    println!("{}\t{}", text(&app, "name"), text(&app, "id"));
                }
            }
        }
        "shell.exec" => {
            let command = arguments.join(" ");
            if command.trim().is_empty() {
                return Err(Failure::Usage(
                    "shell.exec needs a command: shell.exec -- mkdir -p ~/Documents/priv"
                        .to_string(),
                ));
            }
            let mut params = json!({ "command": command });
            if let Some(dir) = scanned.value("cwd") {
                params["cwd"] = json!(dir);
            }
            if let Some(seconds) = scanned.value("timeout") {
                let seconds = seconds.parse::<u64>().map_err(|_| {
                    Failure::Usage(format!("\"{seconds}\" is not a number of seconds"))
                })?;
                params["timeoutSeconds"] = json!(seconds);
            }
            let result = call(&mut client, "shell.exec", Some(&params))?;
            if json {
                print_json(&result)?;
            } else {
                let out = text(&result, "stdout");
                let err = text(&result, "stderr");
                print!("{out}");
                if !err.is_empty() {
                    eprint!("{err}");
                }
                let code = result["exitCode"].as_i64().unwrap_or(-1);
                if code != 0 {
                    return Err(Failure::Failed(format!("the command exited with {code}")));
                }
            }
        }
        "act.launch" => {
            if let Some(app) = scanned.value("app") {
                if !arguments.is_empty() {
                    return Err(Failure::Usage(
                        "act.launch takes either an action or --app <desktop id>, not both"
                            .to_string(),
                    ));
                }
                let result = call(&mut client, "act.launch", Some(&json!({ "app": app })))?;
                if json {
                    print_json(&result)?;
                } else {
                    println!("opened \"{}\"", text(&result, "app"));
                }
                return Ok(());
            }
            let action = arguments.first().ok_or_else(|| {
                Failure::Usage(
                    "act.launch needs an action (browser, terminal, editor, mail) or --app <id>"
                        .to_string(),
                )
            })?;
            if arguments.len() > 1 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\" after the action",
                    arguments[1]
                )));
            }
            let result = call(
                &mut client,
                "act.launch",
                Some(&json!({ "action": action })),
            )?;
            if json {
                print_json(&result)?;
            } else {
                println!("launched \"{}\"", text(&result, "action"));
            }
        }
        "consent" => consent(&mut client, &scanned, &arguments, json)?,
        "audit" => {
            expect_no_arguments(&arguments)?;
            let lines = match scanned.value("lines") {
                None => None,
                Some(text) => {
                    let lines = text
                        .parse::<u64>()
                        .map_err(|_| Failure::Usage(format!("\"{text}\" is not a line count")))?;
                    if !(1..=MAX_TAIL_LINES as u64).contains(&lines) {
                        return Err(Failure::Usage(format!(
                            "--lines must be between 1 and {MAX_TAIL_LINES}"
                        )));
                    }
                    Some(lines)
                }
            };
            let params = match lines {
                Some(lines) => json!({ "lines": lines }),
                None => Value::Null,
            };
            let result = call(&mut client, "audit.tail", Some(&params))?;
            if json {
                print_json(&result)?;
            } else {
                print_audit(&result);
            }
        }
        other => {
            return Err(Failure::Usage(format!(
                "unknown command \"{other}\"; try ping, capabilities, observe.screen, act.type, \
                 act.key, act.clipboard, act.launch, consent or audit"
            )));
        }
    }
    Ok(())
}

/// `act.clipboard read` or `act.clipboard write --text <text>|-`.
fn act_clipboard(
    client: &mut Client,
    scanned: &Scanned,
    arguments: &[String],
    json: bool,
) -> Result<(), Failure> {
    let Some(mode) = arguments.first() else {
        return Err(Failure::Usage(
            "act.clipboard needs \"read\" or \"write\"".to_string(),
        ));
    };
    match mode.as_str() {
        "read" => {
            if arguments.len() > 1 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\"; act.clipboard read takes none",
                    arguments[1]
                )));
            }
            let result = call(client, "act.clipboard", Some(&json!({ "mode": "read" })))?;
            if json {
                print_json(&result)?;
            } else {
                // Printing it is the point of the call: the person asked what is on the clipboard.
                println!("{}", text(&result, "text"));
            }
        }
        "write" => {
            if arguments.len() > 2 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\" after the text",
                    arguments[2]
                )));
            }
            let body = text_argument(scanned, arguments.get(1).map(String::as_str))?;
            let result = call(
                client,
                "act.clipboard",
                Some(&json!({ "mode": "write", "text": body })),
            )?;
            if json {
                print_json(&result)?;
            } else {
                println!(
                    "wrote {} to the clipboard",
                    human_bytes(result.get("bytes").and_then(Value::as_u64).unwrap_or(0))
                );
            }
        }
        other => {
            return Err(Failure::Usage(format!(
                "unknown act.clipboard mode \"{other}\"; try read or write"
            )));
        }
    }
    Ok(())
}

/// The text to act on: `--text <text>`, `--text -` for stdin, or a bare `-` positional.
///
/// A bare `-` positional is what `act.clipboard write -` uses; `--text` wins when both are given.
fn text_argument(scanned: &Scanned, positional: Option<&str>) -> Result<String, Failure> {
    match (scanned.value("text"), positional) {
        (Some("-"), _) | (None, Some("-")) => read_stdin(),
        (Some(text), _) => Ok(text.to_string()),
        (None, Some(text)) => Ok(text.to_string()),
        (None, None) => Err(Failure::Usage(
            "this command needs the text: --text <text>, or --text - to read it from stdin"
                .to_string(),
        )),
    }
}

/// Everything on stdin, verbatim (no trailing newline is stripped), up to one request line.
fn read_stdin() -> Result<String, Failure> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_REQUEST_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|err| Failure::Failed(format!("could not read the text from stdin: {err}")))?;
    String::from_utf8(bytes).map_err(|_| Failure::Failed("stdin is not valid UTF-8".to_string()))
}

fn consent(
    client: &mut Client,
    scanned: &Scanned,
    arguments: &[String],
    json: bool,
) -> Result<(), Failure> {
    let Some(subcommand) = arguments.first() else {
        return Err(Failure::Usage(
            "consent needs a subcommand: grant, revoke or list".to_string(),
        ));
    };
    match subcommand.as_str() {
        "grant" => {
            let capability = arguments.get(1).ok_or_else(|| {
                Failure::Usage("consent grant needs a capability, e.g. observe.screen".to_string())
            })?;
            if arguments.len() > 2 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\" after the capability",
                    arguments[2]
                )));
            }
            let once = scanned.has("once");
            let duration = scanned.value("for");
            let (mode, ttl) = match (once, duration) {
                (true, Some(_)) => {
                    return Err(Failure::Usage(
                        "--once and --for are alternatives; pick one".to_string(),
                    ));
                }
                (false, None) => {
                    return Err(Failure::Usage(
                        "consent grant needs --once (one capture) or --for <duration>".to_string(),
                    ));
                }
                (true, None) => ("once", None),
                (false, Some(text)) => {
                    let seconds = parse_duration(text).map_err(Failure::Usage)?;
                    if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&seconds) {
                        return Err(Failure::Usage(format!(
                            "--for must be between {MIN_TTL_SECONDS}s and {MAX_TTL_SECONDS}s \
                             (got {seconds}s)"
                        )));
                    }
                    ("standing", Some(seconds))
                }
            };
            let mut params = json!({ "capability": capability, "mode": mode });
            if let Some(ttl) = ttl {
                params["ttlSeconds"] = json!(ttl);
            }
            let result = call(client, "consent.grant", Some(&params))?;
            if json {
                print_json(&result)?;
            } else {
                print_grant(&result);
            }
        }
        "revoke" => {
            let capability = arguments.get(1).ok_or_else(|| {
                Failure::Usage("consent revoke needs a capability, e.g. observe.screen".to_string())
            })?;
            if arguments.len() > 2 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\" after the capability",
                    arguments[2]
                )));
            }
            let result = call(
                client,
                "consent.revoke",
                Some(&json!({ "capability": capability })),
            )?;
            if json {
                print_json(&result)?;
            } else {
                print_revoke(&result);
            }
        }
        "list" => {
            if arguments.len() > 1 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\"; consent list takes none",
                    arguments[1]
                )));
            }
            let result = call(client, "consent.list", None)?;
            if json {
                print_json(&result)?;
            } else {
                print_list(&result);
            }
        }
        "pending" => {
            expect_no_arguments(&arguments[1..])?;
            let result = call(client, "consent.pending", None)?;
            if json {
                print_json(&result)?;
            } else {
                print_pending(&result);
            }
        }
        "decide" => {
            let id = arguments.get(1).ok_or_else(|| {
                Failure::Usage(
                    "consent decide needs a pending id (see consent pending)".to_string(),
                )
            })?;
            if arguments.len() > 2 {
                return Err(Failure::Usage(format!(
                    "unexpected argument \"{}\" after the id",
                    arguments[2]
                )));
            }
            let allow = scanned.has("allow");
            let deny = scanned.has("deny");
            if allow == deny {
                return Err(Failure::Usage(
                    "consent decide needs exactly one of --allow or --deny".to_string(),
                ));
            }
            let result = call(
                client,
                "consent.decide",
                Some(&json!({ "id": id, "allow": allow })),
            )?;
            if json {
                print_json(&result)?;
            } else {
                println!("{} {}", text(&result, "decision"), text(&result, "id"));
            }
        }
        "revoke-run" => {
            expect_no_arguments(&arguments[1..])?;
            let result = call(client, "consent.revoke_run", None)?;
            if json {
                print_json(&result)?;
            } else {
                println!("revoked {} run-scoped grants", shown(&result, "revoked"));
            }
        }
        "revoke-all" => {
            expect_no_arguments(&arguments[1..])?;
            let result = call(client, "consent.revoke_all", None)?;
            if json {
                print_json(&result)?;
            } else {
                println!(
                    "revoked {} live grant(s); waiting confirmations were failed",
                    shown(&result, "revoked")
                );
            }
        }
        other => {
            return Err(Failure::Usage(format!(
                "unknown consent subcommand \"{other}\"; try grant, revoke, list, pending, decide, revoke-run or revoke-all"
            )));
        }
    }
    Ok(())
}

fn call(client: &mut Client, method: &str, params: Option<&Value>) -> Result<Value, Failure> {
    client
        .call(method, params)
        .map_err(|RemoteError { code, message }| Failure::Failed(format!("{message} [{code}]")))
}

fn print_costs(report: &CostReport) {
    println!(
        "total {} across {} session{}",
        cost_label(report.total_usd),
        report.sessions.len(),
        if report.sessions.len() == 1 { "" } else { "s" }
    );
    for session in &report.sessions {
        let models = if session.models.is_empty() {
            "no usage".to_string()
        } else {
            session.models.join(", ")
        };
        println!(
            "  {}  {}  {} message{}  {}",
            cost_label(session.cost_usd),
            session.file,
            session.messages,
            if session.messages == 1 { "" } else { "s" },
            models
        );
    }
    if !report.models.is_empty() {
        println!("by model:");
        for model in &report.models {
            println!(
                "  {}  {}  {} message{}",
                cost_label(model.cost_usd),
                model.model,
                model.messages,
                if model.messages == 1 { "" } else { "s" }
            );
        }
    }
}

/// USD with enough decimals to see a fraction of a cent.
fn cost_label(cost: f64) -> String {
    if cost >= 1.0 {
        format!("${cost:.2}")
    } else if cost >= 0.01 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.6}")
    }
}

fn print_json(result: &Value) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(result)
        .map_err(|err| Failure::Failed(format!("could not print the result: {err}")))?;
    println!("{text}");
    Ok(())
}

fn expect_no_arguments(arguments: &[String]) -> Result<(), Failure> {
    match arguments.first() {
        None => Ok(()),
        Some(extra) => Err(Failure::Usage(format!("unexpected argument \"{extra}\""))),
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("?")
}

/// A field for humans: strings as they are, numbers as their decimal form.
fn shown(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => "?".to_string(),
        Some(other) => other.to_string(),
    }
}

fn print_ping(result: &Value, paths: &Paths) {
    println!("omassistantd {}", text(result, "version"));
    println!("  pid        {}", shown(result, "pid"));
    println!("  startedAt  {}", text(result, "startedAt"));
    println!("  socket     {}", paths.socket().display());
}

fn print_capabilities(result: &Value) {
    let empty = Vec::new();
    let capabilities = result
        .get("capabilities")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if capabilities.is_empty() {
        println!("no capabilities");
        return;
    }
    for capability in capabilities {
        println!(
            "{}  {}",
            text(capability, "name"),
            text(capability, "description")
        );
        match capability.get("grant") {
            Some(Value::Null) | None => println!(
                "  grant: none — ask for one with `omassistantctl consent grant {} --once`",
                text(capability, "name")
            ),
            Some(grant) => println!(
                "  grant: {} until {}",
                text(grant, "mode"),
                text(grant, "expiresAt")
            ),
        }
    }
}

fn print_observe(result: &Value) {
    match result.get("window") {
        Some(window) => println!(
            "captured the focused window \"{}\" — \"{}\"",
            text(window, "class"),
            text(window, "title")
        ),
        None => println!("captured the {}", text(result, "target")),
    }
    if let Some(monitor) = result.get("monitor") {
        println!(
            "  monitor {} (scale {})",
            shown(monitor, "name"),
            shown(monitor, "scale")
        );
    }
    println!(
        "  image   {} ({}x{} px, {})",
        text(result, "path"),
        shown(result, "width"),
        shown(result, "height"),
        human_bytes(result.get("bytes").and_then(Value::as_u64).unwrap_or(0))
    );
    if let Some(vision) = result.get("vision") {
        println!(
            "  vision  {} {}x{}, {} (the base64 goes to the caller: pass --json)",
            text(vision, "mime"),
            shown(vision, "width"),
            shown(vision, "height"),
            human_bytes(vision.get("bytes").and_then(Value::as_u64).unwrap_or(0))
        );
    }
    println!("  grant   {}", text(result, "grant"));
}

fn print_grant(result: &Value) {
    println!(
        "granted {}: {} until {} ({} s)",
        text(result, "capability"),
        text(result, "mode"),
        text(result, "expiresAt"),
        shown(result, "ttlSeconds")
    );
}

fn print_pending(result: &Value) {
    let empty = Vec::new();
    let pending = result
        .get("pending")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if pending.is_empty() {
        println!("nothing is waiting");
        return;
    }
    for request in pending {
        println!(
            "{}  {}  {}s left  {}",
            text(request, "id"),
            text(request, "capability"),
            shown(request, "expiresIn"),
            text(request, "summary")
        );
    }
}

fn print_revoke(result: &Value) {
    if result
        .get("revoked")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        println!("revoked {}", text(result, "capability"));
    } else {
        println!("{} had no live grant", text(result, "capability"));
    }
}

fn print_list(result: &Value) {
    let empty = Vec::new();
    let grants = result
        .get("grants")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if grants.is_empty() {
        println!("no grants");
        return;
    }
    for grant in grants {
        let mut extra = String::new();
        if let Some(scope) = grant.get("scope").and_then(Value::as_str) {
            extra.push_str(scope);
        }
        if let Some(target) = grant.get("target").and_then(Value::as_str) {
            extra.push_str(&format!(" in {target}"));
        }
        println!(
            "{}  {}  expires {}{}",
            text(grant, "capability"),
            text(grant, "mode"),
            text(grant, "expiresAt"),
            if extra.is_empty() {
                String::new()
            } else {
                format!("  ({extra})")
            }
        );
    }
}

fn print_audit(result: &Value) {
    let empty = Vec::new();
    let entries = result.as_array().unwrap_or(&empty);
    if entries.is_empty() {
        println!("the audit log has no entries yet");
        return;
    }
    for entry in entries {
        let mut line = format!(
            "{}  {:<7}  {:<16}  grant={}",
            text(entry, "ts"),
            text(entry, "outcome"),
            text(entry, "method"),
            text(entry, "grant")
        );
        for key in ["capability", "target", "app", "geometry", "error"] {
            if let Some(value) = entry.get(key).and_then(Value::as_str) {
                line.push_str(&format!("  {key}={value}"));
            }
        }
        if let Some(bytes) = entry.get("bytes").and_then(Value::as_u64) {
            line.push_str(&format!("  bytes={}", human_bytes(bytes)));
        }
        if let Some(path) = entry.get("path").and_then(Value::as_str) {
            line.push_str(&format!("  path={path}"));
        }
        println!("{line}");
    }
}
