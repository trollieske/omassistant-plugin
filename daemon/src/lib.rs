#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unimplemented,
    clippy::todo,
    clippy::unreachable
)]
//! `omassistantd` — the capability broker behind Omassistant.
//!
//! One user-level process owns every desktop capability. Callers (the shell overlay, an agent CLI,
//! a person with `omassistantctl`) ask that process over a Unix socket with one JSON object per
//! line. The broker answers from three pieces of state, all of which live here:
//!
//! * **grants** — what the person has consented to, and for how long (`grants.json`);
//! * **the audit log** — one JSON line per capability or consent call (`audit/YYYY-MM-DD.jsonl`);
//! * **the captures** — PNG files under `shots/` plus the vision payload returned to the caller.
//!
//! Observed content is untrusted data: the broker never turns a screenshot into an instruction, and
//! it never records a window title, an image, the typed text, the clipboard content or a key chord
//! in the audit log.
//!
//! M2 ships three capabilities — [`caps::OBSERVE_SCREEN`], [`caps::ACT_KEYS`] and
//! [`caps::ACT_CLIPBOARD`] — each with its own grant, its own audit trail and its own caps. There is
//! no `shell.run`, no browser, no mail, no network and no root helper anywhere in this crate.

pub mod act;
pub mod audit;
pub mod base64;
pub mod broker;
pub mod caps;
pub mod capture;
pub mod cli;
pub mod client;
pub mod clock;
pub mod costs;
pub mod error;
pub mod exec;
pub mod grants;
pub mod ids;
pub mod paths;
pub mod protocol;
pub mod server;

/// The daemon version, from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
