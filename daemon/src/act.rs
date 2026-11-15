//! Acting: type text, press a key chord, read or write the clipboard — argv only, never a shell.
//!
//! Three fixed programs, all invoked as argv with a static option set:
//!
//! ```text
//! wtype - < text                        type text into whatever has keyboard focus (stdin)
//! wtype -M logo -k Return -m logo       press one validated key chord
//! wl-copy < text                        put text on the clipboard (stdin, no arguments)
//! wl-paste --no-newline                 read the clipboard back
//! ```
//!
//! Nothing here decides *whether* an action may happen: consent is the broker's job, and this
//! module assumes the caller has already checked it. What this module owns is the part that must
//! never be sloppy — every string that reaches a program is either a constant or validated against
//! a static allowlist first.
//!
//! # The contract with the caller
//!
//! `wtype` types into the **currently focused** window. The broker records which window that was
//! (class for the audit, class and title for the caller), but it cannot focus it: the person's
//! overlay has to close before the keys are sent, or the keys land in the overlay. That ordering is
//! the caller's job; the broker refuses with `no_window` when nothing is focused at all.
//!
//! # The window that was approved is read again
//!
//! A confirmation can wait seconds for an answer, and the person is free to change windows while it
//! waits — so the window the card named is not necessarily the window that has focus when the
//! action runs. Every acting call therefore takes the window it was approved in
//! ([`FocusedWindow`]) and reads the compositor once more immediately before the tool runs,
//! refusing with [`ErrorCode::FocusMoved`] when it is no longer that window. Class and address
//! both have to match, so a second window of the same app is not mistaken for the first. A call
//! that came from a standing grant — nothing was named, nothing waited — passes `None`.
//!
//! The re-check happens in this module, next to the tool, so no caller can skip it by forgetting to
//! ask.
//!
//! # Spelling, verified against wtype 0.4 on this machine
//!
//! * modifiers are `shift`, `capslock`, `ctrl`, `logo`, `win`, `alt`, `altgr` — **not** `super`;
//!   one `-M`/`-m` per modifier (`ctrl+shift` is rejected as a modifier name);
//! * keysym names are looked up case-insensitively by libxkbcommon, but `pageup`/`pagedown` are not
//!   keysyms at all — they are `Prior`/`Next`;
//! * `wtype -` reads the text from stdin; that is how this module types, so the
//!   text never appears in a process's argv.

use crate::error::{ApiError, ErrorCode};
use crate::exec::{CommandRunner, one_line};
use std::sync::Arc;
use std::time::Duration;

/// The most characters `act.type` will type in one call. Characters, not bytes: a pasted document
/// is refused by how long it is to read, not by how wide its encoding happens to be.
pub const MAX_TYPE_CHARS: usize = 4_000;
/// The most characters `act.clipboard write` will put on the clipboard.
pub const MAX_CLIPBOARD_CHARS: usize = 100_000;
/// The fixed launch commands. A name maps to one program; there are no arguments a caller can add,
/// no paths, no shell. `omarchy-launch-*` is the platform's own launcher family.
pub const LAUNCH_ACTIONS: &[(&str, &[&str])] = &[
    ("browser", &["omarchy-launch-browser"]),
    ("terminal", &["omarchy-launch-terminal"]),
    ("editor", &["omarchy-launch-editor"]),
    ("mail", &["betterbird"]),
];

/// How long a launch may take before it is killed.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Validate a shell action name and return it canonical, without running anything.
pub fn shell_action(name: &str) -> Result<&'static str, ApiError> {
    SHELL_ACTIONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(candidate, _)| *candidate)
        .ok_or_else(|| {
            let names: Vec<&str> = SHELL_ACTIONS.iter().map(|(name, _)| *name).collect();
            ApiError::invalid(format!(
                "\"{}\" is not a shell action; choose one of: {}",
                one_line(name, 32),
                names.join(", ")
            ))
        })
}

/// Validate a launch action name and return it canonical, without running anything.
///
/// Consent comes before the launch, so the name is checked here first and the actual start
/// happens in [`Actor::launch`].
pub fn launch_action(name: &str) -> Result<&'static str, ApiError> {
    LAUNCH_ACTIONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(candidate, _)| *candidate)
        .ok_or_else(|| {
            let names: Vec<&str> = LAUNCH_ACTIONS.iter().map(|(name, _)| *name).collect();
            ApiError::invalid(format!(
                "\"{}\" is not a launch action; choose one of: {}",
                one_line(name, 32),
                names.join(", ")
            ))
        })
}

/// Named desktop actions for `shell.run`. Each name is one fixed program with fixed
/// arguments — no caller text, no paths, no shell. Interactive Omarchy helpers
/// (region selection, OCR capture) are deliberately absent: they wait for a person.
pub const SHELL_ACTIONS: &[(&str, &[&str])] = &[
    ("screenshot", &["omarchy-capture-screenshot"]),
    ("lock", &["omarchy-system-lock"]),
    ("volume-up", &["omarchy-audio-output-volume", "raise"]),
    ("volume-down", &["omarchy-audio-output-volume", "lower"]),
    (
        "volume-mute",
        &["omarchy-audio-output-volume", "mute-toggle"],
    ),
    ("brightness-up", &["omarchy-brightness-display", "+5%"]),
    ("brightness-down", &["omarchy-brightness-display", "5%-"]),
    (
        "next-workspace",
        &[
            "hyprctl",
            "dispatch",
            "hl.dsp.focus({ workspace = \"+1\" })",
        ],
    ),
    (
        "previous-workspace",
        &[
            "hyprctl",
            "dispatch",
            "hl.dsp.focus({ workspace = \"-1\" })",
        ],
    ),
    (
        "toggle-layout",
        &["omarchy-hyprland-workspace-layout-toggle"],
    ),
    (
        "fullscreen",
        &[
            "hyprctl",
            "dispatch",
            "hl.dsp.window.fullscreen({ action = \"set\", mode = \"fullscreen\" })",
        ],
    ),
    (
        "maximize",
        &[
            "hyprctl",
            "dispatch",
            "hl.dsp.window.fullscreen({ action = \"set\", mode = \"maximized\" })",
        ],
    ),
    (
        "restore-window",
        &[
            "hyprctl",
            "dispatch",
            "hl.dsp.window.fullscreen({ action = \"unset\" })",
        ],
    ),
];

/// A human sentence for the confirmation and the model.
pub fn shell_action_label(name: &str) -> &'static str {
    match name {
        "screenshot" => "Take a screenshot",
        "lock" => "Lock the screen",
        "volume-up" => "Turn the volume up",
        "volume-down" => "Turn the volume down",
        "volume-mute" => "Mute or unmute the sound",
        "brightness-up" => "Brighten the screen",
        "brightness-down" => "Dim the screen",
        "next-workspace" => "Switch to the next workspace",
        "previous-workspace" => "Switch to the previous workspace",
        "toggle-layout" => "Change the workspace layout",
        "fullscreen" => "Make the active window full screen",
        "maximize" => "Make the active window full width",
        "restore-window" => "Restore the active window",
        _ => "Run a desktop action",
    }
}

/// How long one shell action may take.
pub const SHELL_ACTION_TIMEOUT: Duration = Duration::from_secs(15);

/// The most bytes `act.clipboard read` will hand back.
pub const MAX_CLIPBOARD_BYTES: usize = 100_000;
/// How long one acting tool may run before it is killed.
pub const ACT_TIMEOUT: Duration = Duration::from_secs(5);
/// A chord is one key with at most this many modifiers.
pub const MAX_MODIFIERS: usize = 4;

/// Everything `act.pointer` may do. One action per call; the normalized point is
/// always inside the focused window.
pub const POINTER_ACTIONS: &[&str] = &[
    "click",
    "double-click",
    "right-click",
    "middle-click",
    "scroll",
    "move",
];

/// The most scroll notches one call may send.
pub const MAX_SCROLL_NOTCHES: i32 = 20;

/// Validate a pointer action name and return it canonical.
pub fn pointer_action(action: &str) -> Result<&'static str, ApiError> {
    POINTER_ACTIONS
        .iter()
        .find(|candidate| **candidate == action)
        .copied()
        .ok_or_else(|| {
            ApiError::invalid(format!(
                "\"{}\" is not a pointer action; choose one of: {}",
                one_line(action, 32),
                POINTER_ACTIONS.join(", ")
            ))
        })
}

/// What the compositor says about the focused window, as acting needs it.
///
/// Acting does not care about geometry (it types into whatever has focus), so a window that is
/// unmapped or oddly sized is not an error here — only "there is no focused window" is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedWindow {
    /// The application class. This is the one that goes into the audit log.
    pub class: String,
    /// The compositor's address for this window (`0x…`), or empty when it reports none. Tells two
    /// windows of the same class apart; never logged.
    pub address: String,
    /// The window title. Goes to the caller, never to the audit log.
    pub title: String,
    /// The window's global logical position.
    pub x: i32,
    /// The window's global logical y.
    pub y: i32,
    /// The window's logical width.
    pub width: i32,
    /// The window's logical height.
    pub height: i32,
}

impl FocusedWindow {
    /// Is this the same window as `other`?
    ///
    /// The class is the app the person approved. The address is the exact instance, compared when
    /// both sides have one: a compositor that omits it (or an older Hyprland) still gets the class
    /// check rather than no check at all.
    #[must_use]
    fn same_window(&self, other: &Self) -> bool {
        if self.class != other.class {
            return false;
        }
        if self.address.is_empty() || other.address.is_empty() {
            return true;
        }
        self.address.eq_ignore_ascii_case(&other.address)
    }
}

/// The acting engine: three fixed tools behind an injectable runner.
#[derive(Debug)]
pub struct Actor {
    runner: Arc<dyn CommandRunner>,
}

impl Actor {
    /// Build with an injected runner.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// The focused window's class and title, from `hyprctl -j activewindow`.
    ///
    /// Every way of not getting them (no compositor, no window, an unreadable answer) is reported
    /// as [`ErrorCode::NoWindow`] with the reason in the message: acting has no target without a
    /// focused window, and that is the code a caller can act on.
    pub fn focused_window(&self) -> Result<FocusedWindow, ApiError> {
        let output = self
            .runner
            .run("hyprctl", &["-j", "activewindow"], ACT_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::NoWindow,
                format!(
                    "the compositor could not report the focused window ({}), so there is nothing \
                     to type into",
                    output.failure_note()
                ),
            ));
        }
        parse_focused_window(&output.stdout)
    }

    /// Read the focused window again and require it to still be `approved`.
    ///
    /// Returns the window as it is now, or [`ErrorCode::FocusMoved`] — in which case the caller
    /// must not run the tool. `what` finishes the sentence ("nothing was typed").
    fn recheck_focus(
        &self,
        approved: &FocusedWindow,
        what: &str,
    ) -> Result<FocusedWindow, ApiError> {
        let window = self.focused_window()?;
        if !window.same_window(approved) {
            return Err(ApiError::new(
                ErrorCode::FocusMoved,
                format!(
                    "the focused window changed after that approval, so {what}; approve it again \
                     in the window you mean"
                ),
            ));
        }
        Ok(window)
    }

    /// Start a fixed launch action. No caller-supplied argument ever reaches the command.
    pub fn launch(&self, action: &str) -> Result<(), ApiError> {
        let (name, argv) = LAUNCH_ACTIONS
            .iter()
            .find(|(candidate, _)| *candidate == action)
            .ok_or_else(|| ApiError::invalid("unknown launch action"))?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| ApiError::internal("a launch action has no program"))?;
        // Launch actions are fire-and-forget: the app is meant to outlive this
        // call, so waiting for it would turn a success into a timeout.
        self.runner.spawn_detached(program, args).map_err(|err| {
            ApiError::new(
                ErrorCode::LaunchFailed,
                format!("{} could not start ({})", name, err.message()),
            )
        })
    }

    /// Run a named desktop action. No caller-supplied value reaches the command.
    pub fn run_shell_action(&self, action: &str) -> Result<(), ApiError> {
        let (name, argv) = SHELL_ACTIONS
            .iter()
            .find(|(candidate, _)| *candidate == action)
            .ok_or_else(|| ApiError::invalid("unknown shell action"))?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| ApiError::internal("a shell action has no program"))?;
        let output = self.runner.run(program, args, SHELL_ACTION_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::ActionFailed,
                format!("{} failed ({})", name, output.failure_note()),
            ));
        }
        Ok(())
    }

    /// Type `text` into the focused window with `wtype -` and the text on stdin.
    ///
    /// The text goes over stdin, never argv: `/proc/<pid>/cmdline` is world-readable, and what
    /// someone asks the assistant to type may be private. Validation happens here as well as in the
    /// broker so no caller can hand `wtype` something unchecked.
    ///
    /// `approved` is the window the person confirmed; the compositor is read again and the text is
    /// refused with [`ErrorCode::FocusMoved`] if focus moved on. Validation comes first, so a
    /// refused text never even asks the compositor.
    pub fn type_text(&self, text: &str, approved: Option<&FocusedWindow>) -> Result<(), ApiError> {
        validate_text(text)?;
        if let Some(approved) = approved {
            self.recheck_focus(approved, "nothing was typed")?;
        }
        let output = self
            .runner
            .run_with_input("wtype", &["-"], text.as_bytes(), ACT_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::WtypeFailed,
                format!("wtype could not type the text ({})", output.failure_note()),
            ));
        }
        Ok(())
    }

    /// Press one validated chord: `wtype -M <mod>… -k <key> -m <mod>…` (released in reverse).
    ///
    /// `approved` works as in [`Self::type_text`]: the keys only go out if the approved window is
    /// still the focused one. A key approval is a 60-second run inside one window, so this is the
    /// check that keeps the run from following focus into another app.
    pub fn press(&self, chord: &Chord, approved: Option<&FocusedWindow>) -> Result<(), ApiError> {
        if let Some(approved) = approved {
            self.recheck_focus(approved, "nothing was pressed")?;
        }
        let args = chord.args();
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.runner.run("wtype", &borrowed, ACT_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::WtypeFailed,
                format!("wtype could not press the key ({})", output.failure_note()),
            ));
        }
        Ok(())
    }

    /// Move the real pointer inside the focused window and click, scroll or just
    /// move it. `nx`/`ny` are 0..1 window fractions; the click is a Wayland
    /// virtual-pointer event (`wlrctl`), never uinput or a root daemon.
    ///
    /// `approved` works as in [`Self::type_text`]. The coordinates always come from the window as
    /// it is now, so a window that kept focus but moved is clicked where it actually is.
    pub fn pointer(
        &self,
        action: &str,
        nx: f64,
        ny: f64,
        dx: i32,
        dy: i32,
        approved: Option<&FocusedWindow>,
    ) -> Result<(), ApiError> {
        let action = pointer_action(action)?;
        if !(0.0..=1.0).contains(&nx)
            || !(0.0..=1.0).contains(&ny)
            || !nx.is_finite()
            || !ny.is_finite()
        {
            return Err(ApiError::invalid(
                "\"x\" and \"y\" must be numbers from 0 to 1 (a fraction of the window)",
            ));
        }
        let window = match approved {
            Some(approved) => self.recheck_focus(approved, "nothing was clicked")?,
            None => self.focused_window()?,
        };
        if window.width <= 0 || window.height <= 0 {
            return Err(ApiError::new(
                ErrorCode::NoWindow,
                "the focused window has no area, so there is nowhere to point",
            ));
        }
        let target_x = window
            .x
            .checked_add(((window.width as f64 - 1.0) * nx).round() as i32)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::ActionFailed,
                    "the pointer target is out of range",
                )
            })?;
        let target_y = window
            .y
            .checked_add(((window.height as f64 - 1.0) * ny).round() as i32)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::ActionFailed,
                    "the pointer target is out of range",
                )
            })?;
        let (current_x, current_y) = self.cursor_position()?;
        let step_x = target_x.checked_sub(current_x).ok_or_else(|| {
            ApiError::new(ErrorCode::ActionFailed, "the pointer delta is out of range")
        })?;
        let step_y = target_y.checked_sub(current_y).ok_or_else(|| {
            ApiError::new(ErrorCode::ActionFailed, "the pointer delta is out of range")
        })?;
        self.run_wlrctl(&["pointer", "move", &step_x.to_string(), &step_y.to_string()])?;
        match action {
            "move" => {}
            "click" => self.run_wlrctl(&["pointer", "click", "left"])?,
            "double-click" => {
                self.run_wlrctl(&["pointer", "click", "left"])?;
                std::thread::sleep(Duration::from_millis(40));
                self.run_wlrctl(&["pointer", "click", "left"])?;
            }
            "right-click" => self.run_wlrctl(&["pointer", "click", "right"])?,
            "middle-click" => self.run_wlrctl(&["pointer", "click", "middle"])?,
            "scroll" => {
                if dx == 0 && dy == 0 {
                    return Err(ApiError::invalid(
                        "a scroll needs a non-zero \"dx\" or \"dy\"",
                    ));
                }
                if dx.abs() > MAX_SCROLL_NOTCHES || dy.abs() > MAX_SCROLL_NOTCHES {
                    return Err(ApiError::invalid(format!(
                        "a scroll takes at most {MAX_SCROLL_NOTCHES} notches per axis"
                    )));
                }
                self.run_wlrctl(&["pointer", "scroll", &dx.to_string(), &dy.to_string()])?;
            }
            _ => return Err(ApiError::invalid("unknown pointer action")),
        }
        Ok(())
    }

    fn cursor_position(&self) -> Result<(i32, i32), ApiError> {
        let output = self.runner.run("hyprctl", &["cursorpos"], ACT_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::ActionFailed,
                format!(
                    "the pointer position is not available ({})",
                    output.failure_note()
                ),
            ));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let (x, y) = text.trim().split_once(',').ok_or_else(|| {
            ApiError::new(
                ErrorCode::ActionFailed,
                "the compositor returned a malformed pointer position",
            )
        })?;
        let x = x.trim().parse::<i32>().map_err(|_| {
            ApiError::new(
                ErrorCode::ActionFailed,
                "the compositor returned a malformed pointer position",
            )
        })?;
        let y = y.trim().parse::<i32>().map_err(|_| {
            ApiError::new(
                ErrorCode::ActionFailed,
                "the compositor returned a malformed pointer position",
            )
        })?;
        Ok((x, y))
    }

    fn run_wlrctl(&self, args: &[&str]) -> Result<(), ApiError> {
        let output = self.runner.run("wlrctl", args, ACT_TIMEOUT)?;
        if !output.success() {
            let note = if output.status == Some(127) {
                "wlrctl is not installed; install it for pointer control".to_string()
            } else {
                output.failure_note()
            };
            return Err(ApiError::new(
                ErrorCode::ActionFailed,
                format!("the pointer could not move ({note})"),
            ));
        }
        Ok(())
    }

    /// Put `text` on the clipboard with `wl-copy`, which reads it from stdin.
    ///
    /// Validated here too: the cap belongs to the method that reaches the tool, not only to the
    /// broker that happens to call it first.
    pub fn copy(&self, text: &str) -> Result<(), ApiError> {
        validate_clipboard_text(text)?;
        // Quiet: wl-copy forks the process that serves the selection, and that
        // helper inherits its stdout/stderr. Waiting for EOF there would hang
        // until the clipboard changes; the exit status is the answer.
        let output =
            self.runner
                .run_with_input_quiet("wl-copy", &[], text.as_bytes(), ACT_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::ClipboardFailed,
                format!(
                    "wl-copy could not write to the clipboard ({})",
                    output.failure_note()
                ),
            ));
        }
        Ok(())
    }

    /// Read the clipboard back with `wl-paste --no-newline`, capped at
    /// [`MAX_CLIPBOARD_BYTES`].
    pub fn paste(&self) -> Result<String, ApiError> {
        let output = self
            .runner
            .run("wl-paste", &["--no-newline"], ACT_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::ClipboardFailed,
                format!(
                    "wl-paste could not read the clipboard ({})",
                    output.failure_note()
                ),
            ));
        }
        if output.stdout.len() > MAX_CLIPBOARD_BYTES {
            return Err(ApiError::new(
                ErrorCode::TooLarge,
                format!(
                    "the clipboard holds {} KiB, over the {} KiB this daemon will hand back",
                    output.stdout.len().div_ceil(1024),
                    MAX_CLIPBOARD_BYTES / 1024
                ),
            ));
        }
        // A clipboard can hold anything, including bytes that are not UTF-8; the broker's protocol
        // is JSON, so invalid sequences become U+FFFD rather than a refusal.
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Parse `hyprctl -j activewindow` into what acting needs.
pub fn parse_focused_window(stdout: &[u8]) -> Result<FocusedWindow, ApiError> {
    let value: serde_json::Value = serde_json::from_slice(stdout).map_err(|err| {
        ApiError::new(
            ErrorCode::NoWindow,
            format!("the compositor's active window is not JSON ({err})"),
        )
    })?;
    let object = match value {
        serde_json::Value::Object(object) => object,
        _ => {
            return Err(ApiError::new(
                ErrorCode::NoWindow,
                "the compositor did not describe the active window as an object",
            ));
        }
    };
    if object.is_empty() {
        return Err(ApiError::new(
            ErrorCode::NoWindow,
            "no window is focused, so there is nothing to type into",
        ));
    }
    if object.get("mapped") == Some(&serde_json::Value::Bool(false)) {
        return Err(ApiError::new(
            ErrorCode::NoWindow,
            "the focused window is not mapped, so there is nothing to type into",
        ));
    }
    let text = |key: &str| {
        object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(crate::capture::MAX_TEXT_CHARS)
            .collect::<String>()
    };
    let number = |key: &str, index: usize| -> i32 {
        object
            .get(key)
            .and_then(serde_json::Value::as_array)
            .and_then(|values| values.get(index))
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(0)
    };
    Ok(FocusedWindow {
        class: text("class"),
        address: crate::capture::clamp_address(
            object
                .get("address")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        title: text("title"),
        x: number("at", 0),
        y: number("at", 1),
        width: number("size", 0),
        height: number("size", 1),
    })
}

/// Check the text `act.type` was asked to type, and return its length in characters.
///
/// Refused: nothing to type (empty or whitespace only), any control character other than `\n` and
/// `\t`, and anything longer than [`MAX_TYPE_CHARS`] characters.
pub fn validate_text(text: &str) -> Result<usize, ApiError> {
    if text.trim().is_empty() {
        return Err(ApiError::new(
            ErrorCode::NothingToType,
            "there is nothing to type: the text is empty",
        ));
    }
    if let Some(bad) = text
        .chars()
        .find(|character| character.is_control() && *character != '\n' && *character != '\t')
    {
        return Err(ApiError::invalid(format!(
            "the text contains the control character {}; only newline and tab are allowed \
             (use act.key for keys)",
            one_line(&format!("U+{:04X}", bad as u32), 16)
        )));
    }
    let chars = text.chars().count();
    if chars > MAX_TYPE_CHARS {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!(
                "the text is {chars} characters, over the {MAX_TYPE_CHARS} this daemon will type \
                 in one call"
            ),
        ));
    }
    Ok(chars)
}

/// Check the text `act.clipboard write` was asked to copy, and return its length in characters.
///
/// Unlike `act.type`, empty text is fine — copying nothing is how a person clears the clipboard.
pub fn validate_clipboard_text(text: &str) -> Result<usize, ApiError> {
    let chars = text.chars().count();
    if chars > MAX_CLIPBOARD_CHARS {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!(
                "the text is {chars} characters, over the {MAX_CLIPBOARD_CHARS} this daemon will \
                 put on the clipboard in one call"
            ),
        ));
    }
    Ok(chars)
}

/// One of the four modifiers a chord may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    /// The windows/super key; `wtype` calls it `logo`.
    Super,
    /// Control.
    Ctrl,
    /// Alt.
    Alt,
    /// Shift.
    Shift,
}

impl Modifier {
    /// The name a person writes, and the name this daemon reports back.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Super => "super",
            Self::Ctrl => "ctrl",
            Self::Alt => "alt",
            Self::Shift => "shift",
        }
    }

    /// The spelling `wtype -M` accepts (verified: it rejects `super`).
    pub const fn wtype(self) -> &'static str {
        match self {
            Self::Super => "logo",
            Self::Ctrl => "ctrl",
            Self::Alt => "alt",
            Self::Shift => "shift",
        }
    }

    /// Parse the name a person writes, case-insensitively.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "super" => Some(Self::Super),
            "ctrl" => Some(Self::Ctrl),
            "alt" => Some(Self::Alt),
            "shift" => Some(Self::Shift),
            _ => None,
        }
    }
}

/// One key from the static allowlist: the name a person writes, and the keysym `wtype` wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeySpec {
    name: &'static str,
    keysym: &'static str,
}

/// Every key `act.key` will ever press. Nothing outside this table reaches `wtype`.
const KEYS: &[KeySpec] = &[
    // Letters and digits.
    key("a", "a"),
    key("b", "b"),
    key("c", "c"),
    key("d", "d"),
    key("e", "e"),
    key("f", "f"),
    key("g", "g"),
    key("h", "h"),
    key("i", "i"),
    key("j", "j"),
    key("k", "k"),
    key("l", "l"),
    key("m", "m"),
    key("n", "n"),
    key("o", "o"),
    key("p", "p"),
    key("q", "q"),
    key("r", "r"),
    key("s", "s"),
    key("t", "t"),
    key("u", "u"),
    key("v", "v"),
    key("w", "w"),
    key("x", "x"),
    key("y", "y"),
    key("z", "z"),
    key("0", "0"),
    key("1", "1"),
    key("2", "2"),
    key("3", "3"),
    key("4", "4"),
    key("5", "5"),
    key("6", "6"),
    key("7", "7"),
    key("8", "8"),
    key("9", "9"),
    // Named keys. `Prior`/`Next` are the keysyms for page up/down.
    key("return", "Return"),
    key("space", "space"),
    key("tab", "Tab"),
    key("escape", "Escape"),
    key("backspace", "BackSpace"),
    key("delete", "Delete"),
    key("up", "Up"),
    key("down", "Down"),
    key("left", "Left"),
    key("right", "Right"),
    key("home", "Home"),
    key("end", "End"),
    key("pageup", "Prior"),
    key("pagedown", "Next"),
    key("insert", "Insert"),
    key("f1", "F1"),
    key("f2", "F2"),
    key("f3", "F3"),
    key("f4", "F4"),
    key("f5", "F5"),
    key("f6", "F6"),
    key("f7", "F7"),
    key("f8", "F8"),
    key("f9", "F9"),
    key("f10", "F10"),
    key("f11", "F11"),
    key("f12", "F12"),
    // Punctuation.
    key("minus", "minus"),
    key("equal", "equal"),
    key("comma", "comma"),
    key("period", "period"),
    key("slash", "slash"),
    key("semicolon", "semicolon"),
    key("apostrophe", "apostrophe"),
    key("backslash", "backslash"),
    key("bracketleft", "bracketleft"),
    key("bracketright", "bracketright"),
    key("grave", "grave"),
    key("print", "Print"),
];

const fn key(name: &'static str, keysym: &'static str) -> KeySpec {
    KeySpec { name, keysym }
}

/// One validated key press: zero to [`MAX_MODIFIERS`] modifiers and exactly one allowed key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    modifiers: Vec<Modifier>,
    key: KeySpec,
}

impl Chord {
    /// Parse `super+return`, `ctrl+shift+t`, `escape`.
    ///
    /// Modifier and key names are case-insensitive and surrounding whitespace is
    /// ignored (`ctrl+Home` is the same chord as `ctrl+home`); the canonical
    /// spelling is lowercase.
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        if text.is_empty() {
            return Err(ApiError::invalid(
                "the key chord is empty; write one like \"super+return\" or \"escape\"",
            ));
        }
        let parts: Vec<&str> = text.split('+').collect();
        let (key_part, modifier_parts) = match parts.split_last() {
            Some((key, modifiers)) => (*key, modifiers),
            None => {
                return Err(ApiError::invalid(
                    "the key chord is empty; write one like \"super+return\" or \"escape\"",
                ));
            }
        };
        let mut modifiers: Vec<Modifier> =
            Vec::with_capacity(modifier_parts.len().min(MAX_MODIFIERS));
        for part in modifier_parts {
            let modifier = Modifier::parse(part.trim()).ok_or_else(|| {
                ApiError::invalid(format!(
                    "\"{}\" is not a modifier; use any of super, ctrl, alt, shift",
                    one_line(part, 32)
                ))
            })?;
            if modifiers.len() == MAX_MODIFIERS {
                return Err(ApiError::invalid(format!(
                    "a key chord takes at most {MAX_MODIFIERS} modifiers"
                )));
            }
            if modifiers.contains(&modifier) {
                return Err(ApiError::invalid(format!(
                    "{} is listed twice in the chord",
                    modifier.as_str()
                )));
            }
            modifiers.push(modifier);
        }
        if key_part.is_empty() {
            return Err(ApiError::invalid(
                "the key chord ends with a modifier and no key; write one like \"super+return\"",
            ));
        }
        let key_part = key_part.trim();
        if key_part.is_empty() {
            return Err(ApiError::invalid(
                "the key chord ends with a modifier and no key; write one like \"super+return\"",
            ));
        }
        let key = KEYS
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(key_part));
        let Some(key) = key else {
            return Err(ApiError::invalid(format!(
                "\"{}\" is not a key I can press; use a letter, a digit, or one of return, space, \
                 tab, escape, backspace, delete, up, down, left, right, home, end, pageup, \
                 pagedown, insert, f1..f12, minus, equal, comma, period, slash, semicolon, \
                 apostrophe, backslash, bracketleft, bracketright, grave, print",
                one_line(key_part, 32)
            )));
        };
        Ok(Self {
            modifiers,
            key: *key,
        })
    }

    /// How many keys the press involves: the modifiers plus the key itself.
    ///
    /// This — not the chord — is what the audit log records.
    pub fn key_count(&self) -> usize {
        self.modifiers.len().saturating_add(1)
    }

    /// The canonical spelling of the chord, for the caller's result.
    pub fn canonical(&self) -> String {
        let mut text = String::new();
        for modifier in &self.modifiers {
            text.push_str(modifier.as_str());
            text.push('+');
        }
        text.push_str(self.key.name);
        text
    }

    /// The argv that presses it: modifiers down, key, modifiers up in reverse.
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::with_capacity(self.modifiers.len() * 4 + 2);
        for modifier in &self.modifiers {
            args.push("-M".to_string());
            args.push(modifier.wtype().to_string());
        }
        args.push("-k".to_string());
        args.push(self.key.keysym.to_string());
        for modifier in self.modifiers.iter().rev() {
            args.push("-m".to_string());
            args.push(modifier.wtype().to_string());
        }
        args
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::exec::CommandOutput;
    use std::sync::Mutex;

    /// One recorded call: the program, its argv, and whatever was written to stdin.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Recorded {
        program: String,
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
    }

    /// A runner that records instead of running, and answers from a script.
    #[derive(Debug, Default)]
    struct Recorder {
        calls: Mutex<Vec<Recorded>>,
        window: Mutex<Vec<u8>>,
        cursor: Mutex<String>,
        wtype_fails: Mutex<bool>,
        wtype_times_out: Mutex<bool>,
        wl_copy_fails: Mutex<bool>,
        paste: Mutex<Vec<u8>>,
        paste_fails: Mutex<bool>,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                window: Mutex::new(
                    br#"{"mapped":true,"at":[0,0],"size":[800,600],"class":"foot","title":"t"}"#
                        .to_vec(),
                ),
                cursor: Mutex::new("0, 0".to_string()),
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<Recorded> {
            self.calls.lock().unwrap().clone()
        }

        fn set_window(&self, raw: &[u8]) {
            *self.window.lock().unwrap() = raw.to_vec();
        }

        fn set_cursor(&self, position: &str) {
            *self.cursor.lock().unwrap() = position.to_string();
        }

        fn fail_wtype(&self, timed_out: bool) {
            *self.wtype_fails.lock().unwrap() = true;
            *self.wtype_times_out.lock().unwrap() = timed_out;
        }

        fn fail_copy(&self) {
            *self.wl_copy_fails.lock().unwrap() = true;
        }

        fn set_paste(&self, body: Vec<u8>, fails: bool) {
            *self.paste.lock().unwrap() = body;
            *self.paste_fails.lock().unwrap() = fails;
        }

        fn record(&self, program: &str, args: &[&str], stdin: Option<&[u8]>) {
            self.calls.lock().unwrap().push(Recorded {
                program: program.to_string(),
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                stdin: stdin.map(<[u8]>::to_vec),
            });
        }

        fn answer(&self, program: &str, args: &[&str]) -> CommandOutput {
            match program {
                "hyprctl" if args == ["cursorpos"] => {
                    ok(self.cursor.lock().unwrap().clone().into_bytes())
                }
                "hyprctl" => CommandOutput {
                    status: Some(0),
                    stdout: self.window.lock().unwrap().clone(),
                    stderr: Vec::new(),
                    timed_out: false,
                },
                "wlrctl" => ok(Vec::new()),
                "wtype" => {
                    if *self.wtype_times_out.lock().unwrap() {
                        return CommandOutput {
                            status: None,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                            timed_out: true,
                        };
                    }
                    if *self.wtype_fails.lock().unwrap() {
                        return CommandOutput {
                            status: Some(1),
                            stdout: Vec::new(),
                            stderr: b"wtype: no virtual keyboard".to_vec(),
                            timed_out: false,
                        };
                    }
                    ok(Vec::new())
                }
                "wl-copy" => {
                    if *self.wl_copy_fails.lock().unwrap() {
                        return CommandOutput {
                            status: Some(1),
                            stdout: Vec::new(),
                            stderr: b"wl-copy: no seat".to_vec(),
                            timed_out: false,
                        };
                    }
                    ok(Vec::new())
                }
                "wl-paste" => {
                    if *self.paste_fails.lock().unwrap() {
                        return CommandOutput {
                            status: Some(1),
                            stdout: Vec::new(),
                            stderr: b"wl-paste: Clipboard is empty".to_vec(),
                            timed_out: false,
                        };
                    }
                    ok(self.paste.lock().unwrap().clone())
                }
                other => CommandOutput {
                    status: Some(127),
                    stdout: Vec::new(),
                    stderr: format!("{other}: not installed").into_bytes(),
                    timed_out: false,
                },
            }
        }
    }

    fn ok(stdout: Vec<u8>) -> CommandOutput {
        CommandOutput {
            status: Some(0),
            stdout,
            stderr: Vec::new(),
            timed_out: false,
        }
    }

    impl CommandRunner for Recorder {
        fn run(
            &self,
            program: &str,
            args: &[&str],
            _timeout: Duration,
        ) -> Result<CommandOutput, ApiError> {
            self.record(program, args, None);
            Ok(self.answer(program, args))
        }

        fn run_with_input(
            &self,
            program: &str,
            args: &[&str],
            input: &[u8],
            _timeout: Duration,
        ) -> Result<CommandOutput, ApiError> {
            self.record(program, args, Some(input));
            Ok(self.answer(program, args))
        }

        fn spawn_detached(&self, program: &str, args: &[&str]) -> Result<(), ApiError> {
            self.record(program, args, None);
            Ok(())
        }
    }

    fn actor(recorder: &Arc<Recorder>) -> Actor {
        let runner: Arc<dyn CommandRunner> = recorder.clone();
        Actor::new(runner)
    }

    #[test]
    fn launches_are_fixed_names_and_fixed_programs() {
        for (name, program) in [
            ("browser", "omarchy-launch-browser"),
            ("terminal", "omarchy-launch-terminal"),
            ("editor", "omarchy-launch-editor"),
            ("mail", "betterbird"),
        ] {
            assert_eq!(launch_action(name).unwrap(), name);
            let recorder = Arc::new(Recorder::new());
            actor(&recorder).launch(name).unwrap();
            let calls = recorder.calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].program, program);
            assert!(calls[0].args.is_empty(), "a launch takes no arguments");
        }
        for bad in ["sh", "omarchy-launch-browser", "browser --private", ""] {
            assert!(launch_action(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn every_shell_action_has_a_label_and_a_unique_name() {
        let mut names: Vec<&str> = SHELL_ACTIONS.iter().map(|(name, _)| *name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "shell action names are unique");
        for (name, argv) in SHELL_ACTIONS {
            assert!(!argv.is_empty(), "{name} has a program");
            assert_ne!(
                shell_action_label(name),
                "Run a desktop action",
                "{name} needs a human label"
            );
            assert!(shell_action(name).is_ok());
        }
    }

    #[test]
    fn typing_goes_through_wtype_on_stdin_and_nothing_else() {
        let recorder = Arc::new(Recorder::new());
        actor(&recorder).type_text("hello world", None).unwrap();
        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, "wtype");
        assert_eq!(calls[0].args, vec!["-"]);
        assert_eq!(
            calls[0].stdin.as_deref(),
            Some(b"hello world".as_slice()),
            "the text travels on stdin, never argv"
        );
    }

    #[test]
    fn a_leading_dash_stays_text() {
        let recorder = Arc::new(Recorder::new());
        actor(&recorder).type_text("-hello --json", None).unwrap();
        let calls = recorder.calls();
        assert_eq!(calls[0].args, vec!["-"]);
        assert_eq!(calls[0].stdin.as_deref(), Some(b"-hello --json".as_slice()));
        assert_eq!(calls[0].program, "wtype", "never a shell");
    }

    #[test]
    fn text_validation_counts_characters_and_refuses_hostile_ones() {
        assert_eq!(validate_text("hello").unwrap(), 5);
        assert_eq!(validate_text("two\nlines\tand a tab").unwrap(), 19);
        assert_eq!(validate_text(&"é".repeat(MAX_TYPE_CHARS)).unwrap(), 4000);

        let err = validate_text("").unwrap_err();
        assert_eq!(err.code(), ErrorCode::NothingToType);
        let err = validate_text("  \n\t ").unwrap_err();
        assert_eq!(err.code(), ErrorCode::NothingToType);

        for bad in ["a\u{0}b", "a\u{1b}[31m", "line\r\n", "a\u{7f}"] {
            let err = validate_text(bad).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidParams, "for {bad:?}");
            assert!(err.message().contains("U+"), "{}", err.message());
        }

        // One character over the cap, and the byte/char distinction: 4001 multi-byte characters are
        // 8002 bytes, and still refused for their 4001 characters.
        let err = validate_text(&"x".repeat(MAX_TYPE_CHARS + 1)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::TooLarge);
        assert!(err.message().contains("4001"), "{}", err.message());
        let err = validate_text(&"é".repeat(MAX_TYPE_CHARS + 1)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::TooLarge);
    }

    #[test]
    fn clipboard_text_is_capped_but_may_be_empty() {
        assert_eq!(validate_clipboard_text("").unwrap(), 0);
        assert_eq!(
            validate_clipboard_text(&"é".repeat(MAX_CLIPBOARD_CHARS)).unwrap(),
            100_000
        );
        let err = validate_clipboard_text(&"x".repeat(MAX_CLIPBOARD_CHARS + 1)).unwrap_err();
        assert_eq!(err.code(), ErrorCode::TooLarge);
    }

    #[test]
    fn chords_that_are_allowed() {
        let chord = Chord::parse("super+return").unwrap();
        assert_eq!(chord.canonical(), "super+return");
        assert_eq!(chord.key_count(), 2);
        assert_eq!(
            chord.args(),
            vec!["-M", "logo", "-k", "Return", "-m", "logo"]
        );

        let chord = Chord::parse("ctrl+shift+t").unwrap();
        assert_eq!(chord.canonical(), "ctrl+shift+t");
        assert_eq!(
            chord.args(),
            vec![
                "-M", "ctrl", "-M", "shift", "-k", "t", "-m", "shift", "-m", "ctrl"
            ],
            "modifiers go down in order and come up in reverse"
        );

        let chord = Chord::parse("ctrl+Home").unwrap();
        assert_eq!(chord.canonical(), "ctrl+home");
        assert_eq!(chord.key_count(), 2);
        let chord = Chord::parse(" super+return ").unwrap();
        assert_eq!(chord.canonical(), "super+return");

        let chord = Chord::parse("escape").unwrap();
        assert_eq!(chord.key_count(), 1);
        assert_eq!(chord.args(), vec!["-k", "Escape"]);

        // Modifier and key names are case-insensitive; the canonical spelling is
        // lowercase, so `ctrl+Home` and `ctrl+home` are the same chord.
        assert_eq!(
            Chord::parse("SUPER+Ctrl+escape").unwrap().canonical(),
            "super+ctrl+escape"
        );
        // Four modifiers is the documented ceiling, and it works.
        let chord = Chord::parse("super+ctrl+alt+shift+t").unwrap();
        assert_eq!(chord.key_count(), 5);
        assert_eq!(
            chord.args(),
            vec![
                "-M", "logo", "-M", "ctrl", "-M", "alt", "-M", "shift", "-k", "t", "-m", "shift",
                "-m", "alt", "-m", "ctrl", "-m", "logo"
            ]
        );
        // `pageup`/`pagedown` are not keysyms; they are sent as Prior/Next.
        assert_eq!(
            Chord::parse("ctrl+pageup").unwrap().args(),
            vec!["-M", "ctrl", "-k", "Prior", "-m", "ctrl"]
        );
    }

    #[test]
    fn chords_that_are_refused() {
        for bad in [
            "",
            "+",
            "super+",
            "super++return",
            "hyper+x",
            "super+doesnotexist",
            "a+b",
            "return+a",
            "super+super+return",
            "super+ctrl+alt+shift+shift+t",
        ] {
            let err = Chord::parse(bad).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidParams, "for {bad:?}");
        }
        let err = Chord::parse("hyper+x").unwrap_err();
        assert!(
            err.message().contains("not a modifier"),
            "{}",
            err.message()
        );
        let err = Chord::parse("a+b").unwrap_err();
        assert!(
            err.message().contains("not a modifier"),
            "{}",
            err.message()
        );
        let err = Chord::parse("super+doesnotexist").unwrap_err();
        assert!(err.message().contains("not a key"), "{}", err.message());
    }

    #[test]
    fn the_key_allowlist_is_unique_and_has_no_outside_entries() {
        let mut names: Vec<&str> = KEYS.iter().map(|spec| spec.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two entries share a name");
        assert_eq!(
            total, 75,
            "26 letters + 10 digits + 15 named keys + 12 function keys + 12 punctuation keys"
        );
        for spec in KEYS {
            assert!(
                spec.name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
                "{} is not a lowercase name",
                spec.name
            );
            assert!(
                !spec.keysym.starts_with('-'),
                "{} would look like an option to wtype",
                spec.keysym
            );
        }
    }

    #[test]
    fn pressing_a_chord_sends_exactly_the_validated_argv() {
        let recorder = Arc::new(Recorder::new());
        actor(&recorder)
            .press(&Chord::parse("alt+f4").unwrap(), None)
            .unwrap();
        let calls = recorder.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].program, "wtype");
        assert_eq!(calls[0].args, vec!["-M", "alt", "-k", "F4", "-m", "alt"]);
    }

    #[test]
    fn a_failing_wtype_is_wtype_failed() {
        let recorder = Arc::new(Recorder::new());
        recorder.fail_wtype(false);
        let err = actor(&recorder).type_text("hello", None).unwrap_err();
        assert_eq!(err.code(), ErrorCode::WtypeFailed);
        assert!(
            err.message().contains("no virtual keyboard"),
            "{}",
            err.message()
        );

        let recorder = Arc::new(Recorder::new());
        recorder.fail_wtype(true);
        let err = actor(&recorder)
            .press(&Chord::parse("escape").unwrap(), None)
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::WtypeFailed);
        assert!(err.message().contains("timed out"), "{}", err.message());
    }

    #[test]
    fn the_clipboard_writes_on_stdin_and_reads_without_a_newline() {
        let recorder = Arc::new(Recorder::new());
        let actor = actor(&recorder);
        actor.copy("clip me").unwrap();
        let calls = recorder.calls();
        assert_eq!(calls[0].program, "wl-copy");
        assert!(calls[0].args.is_empty(), "wl-copy takes no arguments here");
        assert_eq!(calls[0].stdin.as_deref(), Some(b"clip me".as_slice()));

        recorder.set_paste(b"from the clipboard".to_vec(), false);
        assert_eq!(actor.paste().unwrap(), "from the clipboard");
        let calls = recorder.calls();
        assert_eq!(calls[1].program, "wl-paste");
        assert_eq!(calls[1].args, vec!["--no-newline"]);
    }

    #[test]
    fn clipboard_failures_and_caps() {
        let recorder = Arc::new(Recorder::new());
        recorder.fail_copy();
        let err = actor(&recorder).copy("x").unwrap_err();
        assert_eq!(err.code(), ErrorCode::ClipboardFailed);
        assert!(err.message().contains("no seat"), "{}", err.message());

        let recorder = Arc::new(Recorder::new());
        recorder.set_paste(Vec::new(), true);
        let err = actor(&recorder).paste().unwrap_err();
        assert_eq!(err.code(), ErrorCode::ClipboardFailed);
        assert!(err.message().contains("empty"), "{}", err.message());

        // Exactly at the cap is fine; one byte over is refused.
        let recorder = Arc::new(Recorder::new());
        recorder.set_paste(vec![b'x'; MAX_CLIPBOARD_BYTES], false);
        assert_eq!(actor(&recorder).paste().unwrap().len(), MAX_CLIPBOARD_BYTES);
        recorder.set_paste(vec![b'x'; MAX_CLIPBOARD_BYTES + 1], false);
        let err = actor(&recorder).paste().unwrap_err();
        assert_eq!(err.code(), ErrorCode::TooLarge);
    }

    #[test]
    fn a_clipboard_that_is_not_utf8_still_comes_back() {
        let recorder = Arc::new(Recorder::new());
        recorder.set_paste(vec![b'a', 0xff, b'b'], false);
        assert_eq!(actor(&recorder).paste().unwrap(), "a\u{fffd}b");
    }

    #[test]
    fn pointer_moves_relative_and_clicks_inside_the_window() {
        let recorder = Arc::new(Recorder::new());
        recorder.set_window(
            br#"{"address":"0x7f","mapped":true,"at":[100,200],"size":[1000,500],"class":"foot","title":"t"}"#,
        );
        recorder.set_cursor("100, 200");
        let approved = actor(&recorder).focused_window().unwrap();
        actor(&recorder)
            .pointer("click", 0.5, 0.5, 0, 0, Some(&approved))
            .unwrap();
        let calls = recorder.calls();
        let wlrctl: Vec<&Recorded> = calls
            .iter()
            .filter(|call| call.program == "wlrctl")
            .collect();
        assert_eq!(wlrctl.len(), 2);
        assert_eq!(wlrctl[0].args, ["pointer", "move", "500", "250"]);
        assert_eq!(wlrctl[1].args, ["pointer", "click", "left"]);
    }

    #[test]
    fn pointer_refuses_a_changed_window_and_bad_points() {
        let recorder = Arc::new(Recorder::new());
        recorder.set_window(
            br#"{"address":"0x1","mapped":true,"at":[0,0],"size":[800,600],"class":"kitty","title":"t"}"#,
        );
        let approved = actor(&recorder).focused_window().unwrap();
        recorder.set_window(
            br#"{"address":"0x2","mapped":true,"at":[0,0],"size":[800,600],"class":"foot","title":"t"}"#,
        );
        let error = actor(&recorder)
            .pointer("click", 0.5, 0.5, 0, 0, Some(&approved))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::FocusMoved);
        assert!(error.message().contains("nothing was clicked"));
        let error = actor(&recorder)
            .pointer("click", 1.5, 0.5, 0, 0, None)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidParams);
        assert!(recorder.calls().iter().all(|call| call.program != "wlrctl"));
    }

    #[test]
    fn pointer_scroll_is_bounded_and_needs_a_delta() {
        let recorder = Arc::new(Recorder::new());
        let error = actor(&recorder)
            .pointer("scroll", 0.5, 0.5, 0, 0, None)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidParams);
        let error = actor(&recorder)
            .pointer("scroll", 0.5, 0.5, 21, 0, None)
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::InvalidParams);
        actor(&recorder)
            .pointer("scroll", 0.5, 0.5, 0, -3, None)
            .unwrap();
        let calls = recorder.calls();
        let scroll = calls
            .iter()
            .find(|call| call.program == "wlrctl" && call.args[1] == "scroll")
            .unwrap();
        assert_eq!(scroll.args, ["pointer", "scroll", "0", "-3"]);
    }

    #[test]
    fn the_focused_window_is_read_and_its_absence_is_no_window() {
        let recorder = Arc::new(Recorder::new());
        let window = actor(&recorder).focused_window().unwrap();
        assert_eq!(window.class, "foot");
        assert_eq!(window.title, "t");
        let calls = recorder.calls();
        assert_eq!(calls[0].program, "hyprctl");
        assert_eq!(calls[0].args, vec!["-j", "activewindow"]);

        for (raw, label) in [
            (&b"{}"[..], "an empty object"),
            (br#"{"mapped":false,"class":"foot"}"#, "an unmapped window"),
            (b"[]", "not an object"),
            (b"oops", "not JSON"),
        ] {
            recorder.set_window(raw);
            let err = actor(&recorder).focused_window().unwrap_err();
            assert_eq!(err.code(), ErrorCode::NoWindow, "for {label}");
        }
    }

    #[test]
    fn the_approved_window_is_read_again_before_typing_or_pressing() {
        // Consent waits for a person; focus does not. The window the card named has to still be
        // the focused one when `wtype` runs.
        let recorder = Arc::new(Recorder::new());
        recorder.set_window(
            br#"{"address":"0x1","mapped":true,"at":[0,0],"size":[800,600],"class":"foot","title":"t"}"#,
        );
        let typing = actor(&recorder);
        let approved = typing.focused_window().unwrap();
        assert_eq!(approved.address, "0x1");

        // The person alt-tabbed while the card was open.
        recorder.set_window(
            br#"{"address":"0x2","mapped":true,"at":[0,0],"size":[800,600],"class":"firefox","title":"t"}"#,
        );
        let error = typing.type_text("hunter2", Some(&approved)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::FocusMoved);
        assert!(
            error.message().contains("nothing was typed"),
            "{}",
            error.message()
        );

        let recorder_no_keys = Arc::new(Recorder::new());
        recorder_no_keys.set_window(
            br#"{"address":"0x1","mapped":true,"at":[0,0],"size":[800,600],"class":"foot","title":"t"}"#,
        );
        let keys = actor(&recorder_no_keys);
        let approved = keys.focused_window().unwrap();
        recorder_no_keys.set_window(
            br#"{"address":"0x2","mapped":true,"at":[0,0],"size":[800,600],"class":"firefox","title":"t"}"#,
        );
        let error = keys
            .press(&Chord::parse("ctrl+w").unwrap(), Some(&approved))
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::FocusMoved);
        assert!(error.message().contains("nothing was pressed"));

        for rec in [&recorder, &recorder_no_keys] {
            assert!(
                rec.calls().iter().all(|call| call.program != "wtype"),
                "nothing reached the keyboard: {:?}",
                rec.calls()
            );
        }
    }

    #[test]
    fn a_second_window_of_the_same_app_is_not_the_approved_one() {
        let recorder = Arc::new(Recorder::new());
        recorder.set_window(
            br#"{"address":"0xaa","mapped":true,"at":[0,0],"size":[800,600],"class":"foot","title":"one"}"#,
        );
        let actor = actor(&recorder);
        let approved = actor.focused_window().unwrap();
        // Same app, other window: the class matches, the address does not.
        recorder.set_window(
            br#"{"address":"0xbb","mapped":true,"at":[0,0],"size":[800,600],"class":"foot","title":"two"}"#,
        );
        assert_eq!(
            actor
                .type_text("hello", Some(&approved))
                .unwrap_err()
                .code(),
            ErrorCode::FocusMoved
        );
        // Hyprland's addresses are hex; case is not identity.
        recorder.set_window(
            br#"{"address":"0xAA","mapped":true,"at":[0,0],"size":[800,600],"class":"foot","title":"one"}"#,
        );
        actor.type_text("hello", Some(&approved)).unwrap();
    }

    #[test]
    fn a_compositor_without_an_address_still_gets_the_class_check() {
        let recorder = Arc::new(Recorder::new());
        let actor = actor(&recorder);
        let approved = actor.focused_window().unwrap();
        assert_eq!(approved.address, "", "this fake window reports none");
        // No address to compare: the class still has to match, so another app is refused…
        recorder.set_window(
            br#"{"mapped":true,"at":[0,0],"size":[100,100],"class":"kitty","title":"t"}"#,
        );
        assert_eq!(
            actor
                .type_text("hello", Some(&approved))
                .unwrap_err()
                .code(),
            ErrorCode::FocusMoved
        );
        // …while the same app (this is all the compositor tells us) is allowed.
        recorder.set_window(
            br#"{"mapped":true,"at":[40,40],"size":[100,100],"class":"foot","title":"moved"}"#,
        );
        actor.type_text("hello", Some(&approved)).unwrap();
    }

    #[test]
    fn the_engine_itself_refuses_text_it_was_never_supposed_to_see() {
        // Defence in depth: the broker validates first, but the code that reaches the tool does
        // too, so no future caller can hand wtype or wl-copy something unchecked.
        let recorder = Arc::new(Recorder::new());
        let actor = actor(&recorder);
        assert_eq!(
            actor.type_text("", None).unwrap_err().code(),
            ErrorCode::NothingToType
        );
        assert_eq!(
            actor.type_text("a\u{0}b", None).unwrap_err().code(),
            ErrorCode::InvalidParams
        );
        assert_eq!(
            actor
                .type_text(&"x".repeat(MAX_TYPE_CHARS + 1), None)
                .unwrap_err()
                .code(),
            ErrorCode::TooLarge
        );
        assert_eq!(
            actor
                .copy(&"x".repeat(MAX_CLIPBOARD_CHARS + 1))
                .unwrap_err()
                .code(),
            ErrorCode::TooLarge
        );
        assert!(
            recorder.calls().is_empty(),
            "nothing reached a tool: {:?}",
            recorder.calls()
        );
    }

    #[test]
    fn a_hostile_title_is_clamped() {
        let long = "t".repeat(10_000);
        let raw = format!(r#"{{"mapped":true,"class":"kitty","title":"{long}"}}"#);
        let window = parse_focused_window(raw.as_bytes()).unwrap();
        assert_eq!(window.title.chars().count(), crate::capture::MAX_TEXT_CHARS);
    }

    #[test]
    fn a_hostile_address_is_clamped_and_stripped() {
        let raw = format!(
            r#"{{"mapped":true,"class":"kitty","address":"0x\u001b\u0000{}"}}"#,
            "f".repeat(10_000)
        );
        let window = parse_focused_window(raw.as_bytes()).unwrap();
        assert_eq!(
            window.address.chars().count(),
            crate::capture::MAX_ADDRESS_CHARS
        );
        assert!(!window.address.contains('\u{1b}') && !window.address.contains('\u{0}'));
        assert!(window.address.starts_with("0x"), "{}", window.address);
    }
}
