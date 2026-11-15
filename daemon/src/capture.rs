//! Observation: geometry from the compositor, one capture with `grim`, one vision payload.
//!
//! # Coordinates (Omarchy, Hyprland, scale 2)
//!
//! `hyprctl -j activewindow` reports **logical** coordinates — the same space Hyprland uses for
//! `-g` — and `grim -g "x,y wxh"` accepts that space directly, rendering the result at the
//! monitor's scale:
//!
//! ```text
//! hyprctl activewindow  →  at 100,50  size 800x600     (monitor DP-1, scale 2)
//! grim -g "100,50 800x600" shot.png    →  PNG 1600x1200  (= 2 × the logical rect)
//! grim -o DP-1 shot.png                →  PNG 1920x1080  (the monitor's physical pixels)
//! grim -o DP-1 -s 1.0 shot.png         →  PNG 960x540
//! ```
//!
//! So the scale is **not** applied by us: no dividing, no rounding, no multiplying. It is reported
//! to the caller because the PNG on disk is `scale ×` the geometry we asked for, and because an
//! agent comparing pixel coordinates with `hyprctl` output needs to know that.
//!
//! Everything in this module is argv-only. `grim` and `hyprctl` are the fixed programs; nothing is
//! ever handed to a shell.

use crate::base64;
use crate::error::{ApiError, ErrorCode};
use crate::exec::{CommandRunner, MAX_MESSAGE_CHARS, one_line, which};
use crate::ids;
use crate::paths::FILE_MODE;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// A capture may not take longer than this; the child is killed when it does.
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
/// A vision conversion may not take longer than this.
pub const CONVERT_TIMEOUT: Duration = Duration::from_secs(20);
/// No capture dimension may exceed this; anything larger is a compositor bug or a hostile number.
pub const MAX_DIMENSION: u32 = 65_535;
/// The most raw image data this daemon will read for a vision payload (3 MiB → 4 MiB of base64).
pub const MAX_IMAGE_BYTES: u64 = 3 * 1024 * 1024;
/// The most base64 this daemon will put in one result (4 MiB).
pub const MAX_BASE64_BYTES: usize = 4 * 1024 * 1024;
/// The long edge of the vision image, in pixels.
pub const VISION_MAX_EDGE: u32 = 1280;
/// JPEG quality for the vision image.
pub const VISION_QUALITY: u32 = 82;
/// The longest class or title this daemon will echo back.
pub const MAX_TEXT_CHARS: usize = 512;

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    /// The focused window's rectangle.
    Window,
    /// The focused monitor.
    Screen,
}

impl Target {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Screen => "screen",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        match text {
            "window" => Ok(Self::Window),
            "screen" => Ok(Self::Screen),
            other => Err(ApiError::invalid(format!(
                "\"target\" must be \"window\" or \"screen\", not \"{}\"",
                one_line(other, 32)
            ))),
        }
    }
}

/// A rectangle in the compositor's logical coordinates. `x`/`y` may be negative on a multi-monitor
/// layout; the size may not be zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width in logical pixels.
    pub width: u32,
    /// Height in logical pixels.
    pub height: u32,
}

impl Rect {
    /// The geometry string `grim -g` wants.
    pub fn geometry(&self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

/// What the compositor says about the focused window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFacts {
    /// The application class (never logged as a title).
    pub class: String,
    /// The compositor's address for this window (`0x…`), or empty when it reports none. The exact
    /// identity of the window a confirmation named; never logged.
    pub address: String,
    /// The window title. Goes to the caller, never to the audit log.
    pub title: String,
    /// The window rectangle.
    pub rect: Rect,
    /// The index of the monitor the window is on, when the compositor says.
    pub monitor: Option<usize>,
}

impl WindowFacts {
    /// Is this still the same window as `other`?
    ///
    /// The class is the app the person approved; the address narrows it to the instance when both
    /// reads have one, so a second window of the same app is never captured under the first one's
    /// approval.
    #[must_use]
    pub fn same_window(&self, other: &Self) -> bool {
        if self.class != other.class {
            return false;
        }
        if self.address.is_empty() || other.address.is_empty() {
            return true;
        }
        self.address.eq_ignore_ascii_case(&other.address)
    }
}

/// What the compositor says about one monitor.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorFacts {
    /// Output name, e.g. `eDP-1`.
    pub name: String,
    /// Scale factor; finite and greater than zero.
    pub scale: f64,
    /// Width in physical pixels, as the compositor reports it.
    pub width: u32,
    /// Height in physical pixels, as the compositor reports it.
    pub height: u32,
    /// Whether this is the focused monitor.
    pub focused: bool,
}

/// The image handed to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vision {
    /// `image/jpeg` when ImageMagick converted it, `image/png` otherwise.
    pub mime: &'static str,
    /// Width of the encoded image.
    pub width: u32,
    /// Height of the encoded image.
    pub height: u32,
    /// Size of the encoded image in bytes.
    pub bytes: u64,
    /// The encoded image.
    pub base64: String,
}

/// Which optional tools this daemon found. Resolved once, injectable in tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSet {
    /// ImageMagick 7's `magick`, when it is on `PATH`.
    pub magick: Option<PathBuf>,
}

impl ToolSet {
    /// Look for the optional tools on `PATH`.
    pub fn detect() -> Self {
        Self {
            magick: which("magick"),
        }
    }

    /// Pretend nothing optional is installed (the daemon still works — the vision payload is then
    /// the PNG itself).
    pub fn none() -> Self {
        Self { magick: None }
    }
}

/// The capture engine: compositor facts in, PNG on disk plus a vision payload out.
#[derive(Debug)]
pub struct Capturer {
    runner: Arc<dyn CommandRunner>,
    tools: ToolSet,
}

impl Capturer {
    /// Build with an injected runner and a resolved tool set.
    pub fn new(runner: Arc<dyn CommandRunner>, tools: ToolSet) -> Self {
        Self { runner, tools }
    }

    /// The focused window's facts, from `hyprctl -j activewindow`.
    pub fn active_window(&self) -> Result<WindowFacts, ApiError> {
        let output = self
            .runner
            .run("hyprctl", &["-j", "activewindow"], CAPTURE_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::CaptureFailed,
                format!(
                    "hyprctl could not report the focused window ({})",
                    output.failure_note()
                ),
            ));
        }
        parse_active_window(&output.stdout)
    }

    /// Every monitor, from `hyprctl -j monitors`.
    pub fn monitors(&self) -> Result<Vec<MonitorFacts>, ApiError> {
        let output = self
            .runner
            .run("hyprctl", &["-j", "monitors"], CAPTURE_TIMEOUT)?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::CaptureFailed,
                format!(
                    "hyprctl could not report the monitors ({})",
                    output.failure_note()
                ),
            ));
        }
        parse_monitors(&output.stdout)
    }

    /// The monitor that has focus.
    pub fn focused_monitor(&self) -> Result<MonitorFacts, ApiError> {
        let monitors = self.monitors()?;
        monitors
            .into_iter()
            .find(|monitor| monitor.focused)
            .ok_or_else(|| {
                ApiError::new(
                    ErrorCode::NoMonitor,
                    "no monitor reports focus, so there is no screen to capture",
                )
            })
    }

    /// Capture a window rectangle with `grim -g`.
    pub fn grab_window(&self, window: &WindowFacts, path: &Path) -> Result<(), ApiError> {
        let geometry = window.rect.geometry();
        let path_text = path_text(path)?;
        let output = self.runner.run(
            "grim",
            &["-g", geometry.as_str(), path_text],
            CAPTURE_TIMEOUT,
        )?;
        if !output.success() {
            return Err(ApiError::new(
                ErrorCode::CaptureFailed,
                format!(
                    "grim could not capture the window at {geometry} ({})",
                    output.failure_note()
                ),
            ));
        }
        require_written(path)
    }

    /// Capture a whole monitor with `grim -o`, falling back to "the whole layout" with plain grim.
    pub fn grab_screen(&self, monitor: &MonitorFacts, path: &Path) -> Result<(), ApiError> {
        let path_text = path_text(path)?;
        let output = self.runner.run(
            "grim",
            &["-o", monitor.name.as_str(), path_text],
            CAPTURE_TIMEOUT,
        )?;
        if output.success() {
            return require_written(path);
        }
        // No whole-layout fallback: that would capture every monitor, which is
        // wider than the audit entry and the consent describe. Refuse instead.
        Err(ApiError::new(
            ErrorCode::CaptureFailed,
            format!(
                "grim could not capture the monitor {} ({})",
                one_line(&monitor.name, MAX_MESSAGE_CHARS),
                output.failure_note()
            ),
        ))
    }

    /// Turn the PNG into the smallest useful image for the agent.
    ///
    /// With `magick` available this is a JPEG (long edge ≤ 1280 px, quality 82) made in a scratch
    /// file under `shots/`, read, and deleted. Without `magick` — or if the conversion fails — the
    /// PNG itself is the payload. Either way the payload is capped at 4 MiB of base64.
    pub fn vision(&self, png: &Path, scratch_dir: &Path) -> Result<Vision, ApiError> {
        if let Some(magick) = self.tools.magick.as_deref() {
            if let Some(vision) = self.convert(magick, png, scratch_dir)? {
                return Ok(vision);
            }
        }
        let raw = read_capped(png, MAX_IMAGE_BYTES)?;
        let (width, height) = png_dimensions(&raw)?;
        let base64 = encode_capped(&raw)?;
        Ok(Vision {
            mime: "image/png",
            width,
            height,
            bytes: raw.len() as u64,
            base64,
        })
    }

    /// The ImageMagick path. `Ok(None)` means "not available, use the PNG".
    fn convert(
        &self,
        magick: &Path,
        png: &Path,
        scratch_dir: &Path,
    ) -> Result<Option<Vision>, ApiError> {
        let program = match magick.to_str() {
            Some(program) => program,
            None => return Ok(None),
        };
        let png_text = path_text(png)?;
        let scratch = scratch_dir.join(format!("vision-{}.jpg", ids::random_hex(6)));
        let scratch_text = path_text(&scratch)?;
        let size = format!("{VISION_MAX_EDGE}x{VISION_MAX_EDGE}>");
        let quality = VISION_QUALITY.to_string();
        let result = self.runner.run(
            program,
            &[
                png_text,
                "-resize",
                size.as_str(),
                "-quality",
                quality.as_str(),
                "-write",
                scratch_text,
                "-format",
                "%wx%h",
                "info:",
            ],
            CONVERT_TIMEOUT,
        );
        let output = match result {
            Ok(output) => output,
            Err(_) => {
                let _ = fs::remove_file(&scratch);
                return Ok(None);
            }
        };
        if !output.success() {
            let _ = fs::remove_file(&scratch);
            return Ok(None);
        }
        // Without the real dimensions the vision block would be a guess, so an unparseable
        // `%wx%h` counts as a failed conversion and the PNG is used instead.
        let dims = parse_dimensions_text(&output.stdout_text());
        let raw = read_capped(&scratch, MAX_IMAGE_BYTES);
        let _ = fs::remove_file(&scratch);
        let raw = raw?;
        let Some((width, height)) = dims else {
            return Ok(None);
        };
        let base64 = encode_capped(&raw)?;
        Ok(Some(Vision {
            mime: "image/jpeg",
            width,
            height,
            bytes: raw.len() as u64,
            base64,
        }))
    }
}

fn path_text(path: &Path) -> Result<&str, ApiError> {
    path.to_str().ok_or_else(|| {
        ApiError::internal(format!(
            "{} is not valid UTF-8, so it cannot be passed to a capture tool",
            path.display()
        ))
    })
}

/// Confirm the tool wrote something, and make the file private to the person.
///
/// `grim` creates its output with the process umask, which is usually 0644; a screenshot of the
/// person's screen is theirs alone, so it is chmodded 0600 here, at the one place that knows a
/// capture just landed.
fn require_written(path: &Path) -> Result<(), ApiError> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => crate::paths::set_mode(path, FILE_MODE),
        Ok(meta) => Err(ApiError::new(
            ErrorCode::CaptureFailed,
            format!(
                "the capture tool reported success but wrote {} bytes to {}",
                meta.len(),
                path.display()
            ),
        )),
        Err(err) => Err(ApiError::new(
            ErrorCode::CaptureFailed,
            format!(
                "the capture tool reported success but {} is unreadable: {err}",
                path.display()
            ),
        )),
    }
}

/// Read at most `limit` bytes, refusing outright when the file is bigger.
fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>, ApiError> {
    let meta = fs::metadata(path).map_err(|err| {
        ApiError::internal(format!("could not inspect {}: {err}", path.display()))
    })?;
    if meta.len() > limit {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!(
                "{} is {} KiB, over the {} KiB limit for a vision payload",
                path.display(),
                meta.len().div_ceil(1024),
                limit / 1024
            ),
        ));
    }
    Ok(fs::read(path)?)
}

/// Base64 with the protocol's cap applied before the string is allocated.
fn encode_capped(raw: &[u8]) -> Result<String, ApiError> {
    if base64::encoded_len(raw.len()) > MAX_BASE64_BYTES {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!(
                "the encoded image would be {} KiB, over the {} KiB limit",
                base64::encoded_len(raw.len()).div_ceil(1024),
                MAX_BASE64_BYTES / 1024
            ),
        ));
    }
    let encoded = base64::encode(raw);
    if encoded.len() > MAX_BASE64_BYTES {
        return Err(ApiError::new(
            ErrorCode::TooLarge,
            format!(
                "the encoded image is {} KiB, over the {} KiB limit",
                encoded.len().div_ceil(1024),
                MAX_BASE64_BYTES / 1024
            ),
        ));
    }
    Ok(encoded)
}

/// Parse `hyprctl -j activewindow` output.
pub fn parse_active_window(stdout: &[u8]) -> Result<WindowFacts, ApiError> {
    let value: Value = serde_json::from_slice(stdout).map_err(|err| {
        ApiError::new(
            ErrorCode::CaptureFailed,
            format!("hyprctl activewindow did not return JSON ({err})"),
        )
    })?;
    let object = match value {
        Value::Object(object) => object,
        _ => {
            return Err(ApiError::new(
                ErrorCode::CaptureFailed,
                "hyprctl activewindow did not return an object",
            ));
        }
    };
    if object.is_empty() {
        return Err(ApiError::new(
            ErrorCode::NoWindow,
            "no window is focused, so there is nothing to capture",
        ));
    }
    if object.get("mapped") == Some(&Value::Bool(false)) {
        return Err(ApiError::new(
            ErrorCode::NoWindow,
            "the focused window is not mapped, so there is nothing to capture",
        ));
    }
    let at = object.get("at").ok_or_else(|| {
        ApiError::new(
            ErrorCode::CaptureFailed,
            "hyprctl activewindow did not report a position",
        )
    })?;
    let size = object.get("size").ok_or_else(|| {
        ApiError::new(
            ErrorCode::CaptureFailed,
            "hyprctl activewindow did not report a size",
        )
    })?;
    let x = pair_component_i32(at, 0, "position")?;
    let y = pair_component_i32(at, 1, "position")?;
    let width = pair_component_u32(size, 0, "size")?;
    let height = pair_component_u32(size, 1, "size")?;
    let rect = validate_rect(x, y, width, height)?;
    let class = clamp_text(string_field(object.get("class")));
    let address = clamp_address(string_field(object.get("address")));
    let title = clamp_text(string_field(object.get("title")));
    let monitor = object
        .get("monitor")
        .and_then(Value::as_i64)
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index <= 64);
    Ok(WindowFacts {
        class,
        address,
        title,
        rect,
        monitor,
    })
}

/// Parse `hyprctl -j monitors` output.
pub fn parse_monitors(stdout: &[u8]) -> Result<Vec<MonitorFacts>, ApiError> {
    let value: Value = serde_json::from_slice(stdout).map_err(|err| {
        ApiError::new(
            ErrorCode::CaptureFailed,
            format!("hyprctl monitors did not return JSON ({err})"),
        )
    })?;
    let array = match value {
        Value::Array(array) => array,
        _ => {
            return Err(ApiError::new(
                ErrorCode::CaptureFailed,
                "hyprctl monitors did not return an array",
            ));
        }
    };
    if array.is_empty() {
        return Err(ApiError::new(
            ErrorCode::NoMonitor,
            "the compositor reports no monitors, so there is no screen to capture",
        ));
    }
    let mut monitors = Vec::with_capacity(array.len().min(16));
    for entry in array {
        let object = match entry {
            Value::Object(object) => object,
            _ => {
                return Err(ApiError::new(
                    ErrorCode::CaptureFailed,
                    "hyprctl monitors returned an entry that is not an object",
                ));
            }
        };
        let name = string_field(object.get("name"));
        if name.is_empty() {
            return Err(ApiError::new(
                ErrorCode::CaptureFailed,
                "a monitor has no name, so grim cannot be pointed at it",
            ));
        }
        let scale = match object.get("scale").and_then(Value::as_f64) {
            Some(scale) if scale.is_finite() && scale > 0.0 && scale <= 16.0 => scale,
            _ => {
                return Err(ApiError::new(
                    ErrorCode::InvalidGeometry,
                    format!(
                        "monitor {} reports an unusable scale (must be a finite number above 0)",
                        one_line(&name, MAX_MESSAGE_CHARS)
                    ),
                ));
            }
        };
        let width = object
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let height = object
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let focused = object
            .get("focused")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        monitors.push(MonitorFacts {
            name,
            scale,
            width: width.min(MAX_DIMENSION),
            height: height.min(MAX_DIMENSION),
            focused,
        });
    }
    Ok(monitors)
}

fn string_field(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The longest window address this daemon will carry. Hyprland sends `0x` and a pointer-sized hex
/// number; the cap exists only because the field comes from the compositor.
pub const MAX_ADDRESS_CHARS: usize = 64;

/// Keep the first `MAX_ADDRESS_CHARS` characters of a window address, without control characters.
pub(crate) fn clamp_address(text: String) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_ADDRESS_CHARS)
        .collect()
}

/// Keep the first `MAX_TEXT_CHARS` characters, on a character boundary.
fn clamp_text(text: String) -> String {
    if text.chars().count() <= MAX_TEXT_CHARS {
        return text;
    }
    text.chars().take(MAX_TEXT_CHARS).collect()
}

fn pair_component_i32(pair: &Value, index: usize, what: &str) -> Result<i32, ApiError> {
    let value = pair
        .as_array()
        .and_then(|array| array.get(index))
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::CaptureFailed,
                format!("hyprctl reported a {what} that is not a pair of whole numbers"),
            )
        })?;
    i32::try_from(value).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidGeometry,
            format!("the reported {what} ({value}) does not fit in a 32-bit coordinate"),
        )
    })
}

fn pair_component_u32(pair: &Value, index: usize, what: &str) -> Result<u32, ApiError> {
    let value = pair_component_i32(pair, index, what)?;
    u32::try_from(value).map_err(|_| {
        ApiError::new(
            ErrorCode::InvalidGeometry,
            format!("the reported {what} ({value}) is negative"),
        )
    })
}

/// Check a rectangle before it is handed to `grim`.
pub fn validate_rect(x: i32, y: i32, width: u32, height: u32) -> Result<Rect, ApiError> {
    if width == 0 || height == 0 {
        return Err(ApiError::new(
            ErrorCode::InvalidGeometry,
            format!("the capture rectangle is {width}x{height}, which is empty"),
        ));
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(ApiError::new(
            ErrorCode::InvalidGeometry,
            format!(
                "the capture rectangle is {width}x{height}, larger than the {MAX_DIMENSION} px limit"
            ),
        ));
    }
    Ok(Rect {
        x,
        y,
        width,
        height,
    })
}

/// Width and height out of a PNG's IHDR chunk.
pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), ApiError> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    if bytes.len() < 24 {
        return Err(ApiError::new(
            ErrorCode::CaptureFailed,
            "the capture is too short to be a PNG",
        ));
    }
    if bytes[..8] != SIGNATURE {
        return Err(ApiError::new(
            ErrorCode::CaptureFailed,
            "the capture is not a PNG",
        ));
    }
    if &bytes[12..16] != b"IHDR" {
        return Err(ApiError::new(
            ErrorCode::CaptureFailed,
            "the capture has no PNG header chunk",
        ));
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if width == 0 || height == 0 {
        return Err(ApiError::new(
            ErrorCode::CaptureFailed,
            "the capture is an empty image",
        ));
    }
    Ok((width, height))
}

/// Read just the IHDR chunk of a PNG on disk.
pub fn png_file_dimensions(path: &Path) -> Result<(u32, u32), ApiError> {
    use std::io::Read;
    let mut header = [0u8; 24];
    let mut file = fs::File::open(path)?;
    file.read_exact(&mut header).map_err(|err| {
        ApiError::new(
            ErrorCode::CaptureFailed,
            format!("could not read the PNG header of {}: {err}", path.display()),
        )
    })?;
    png_dimensions(&header)
}

/// Parse `magick`'s `%wx%h` output, e.g. `1280x752`.
fn parse_dimensions_text(text: &str) -> Option<(u32, u32)> {
    let (width, height) = text.trim().split_once('x')?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A minimal PNG header: signature, IHDR length/type, width, height, five more IHDR bytes.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, colour type, …
        bytes
    }

    #[test]
    fn window_facts_come_from_hyprctl() {
        let raw = br#"{"address":"0x55","mapped":true,"at":[100,50],"size":[800,600],
                       "class":"foot","title":"example: ~","monitor":0,"fullscreen":1}"#;
        let facts = parse_active_window(raw).unwrap();
        assert_eq!(
            facts.rect,
            Rect {
                x: 100,
                y: 50,
                width: 800,
                height: 600
            }
        );
        assert_eq!(facts.class, "foot");
        assert_eq!(facts.title, "example: ~");
        assert_eq!(facts.monitor, Some(0));
        assert_eq!(facts.rect.geometry(), "100,50 800x600");
    }

    #[test]
    fn negative_coordinates_are_fine() {
        let raw = br#"{"at":[-1920,-40],"size":[800,600],"class":"kitty","title":"t"}"#;
        let facts = parse_active_window(raw).unwrap();
        assert_eq!(facts.rect.geometry(), "-1920,-40 800x600");
    }

    #[test]
    fn no_window_is_its_own_error() {
        let err = parse_active_window(b"{}").unwrap_err();
        assert_eq!(err.code(), ErrorCode::NoWindow);
        let err = parse_active_window(br#"{"address":"0x0","class":"","title":""}"#).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CaptureFailed);
        let err =
            parse_active_window(br#"{"mapped":false,"at":[0,0],"size":[10,10]}"#).unwrap_err();
        assert_eq!(err.code(), ErrorCode::NoWindow);
    }

    #[test]
    fn hostile_geometry_is_refused() {
        let cases: &[(&[u8], ErrorCode)] = &[
            // zero size
            (
                br#"{"at":[0,0],"size":[0,600]}"#,
                ErrorCode::InvalidGeometry,
            ),
            // negative size
            (
                br#"{"at":[0,0],"size":[-10,600]}"#,
                ErrorCode::InvalidGeometry,
            ),
            // absurd size
            (
                br#"{"at":[0,0],"size":[100000,600]}"#,
                ErrorCode::InvalidGeometry,
            ),
            // coordinate out of i32
            (
                br#"{"at":[4294967296,0],"size":[10,10]}"#,
                ErrorCode::InvalidGeometry,
            ),
            // not a pair
            (br#"{"at":"0,0","size":[10,10]}"#, ErrorCode::CaptureFailed),
            // not an object
            (br#"[1,2,3]"#, ErrorCode::CaptureFailed),
            // not JSON
            (b"oops", ErrorCode::CaptureFailed),
        ];
        for (raw, expected) in cases {
            let err = parse_active_window(raw).unwrap_err();
            assert_eq!(err.code(), *expected, "for {raw:?}: {err}");
        }
    }

    #[test]
    fn long_titles_are_clamped() {
        let long = "x".repeat(10_000);
        let raw = format!(r#"{{"at":[0,0],"size":[10,10],"class":"kitty","title":"{long}"}}"#);
        let facts = parse_active_window(raw.as_bytes()).unwrap();
        assert_eq!(facts.title.chars().count(), MAX_TEXT_CHARS);
    }

    #[test]
    fn monitors_are_parsed_and_scales_validated() {
        let raw = br#"[{"name":"DP-1","width":1920,"height":1080,"x":0,"y":0,
                        "scale":2,"focused":true},
                       {"name":"HDMI-A-1","width":1280,"height":720,"scale":1.0,"focused":false}]"#;
        let monitors = parse_monitors(raw).unwrap();
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].name, "DP-1");
        assert_eq!(monitors[0].scale, 2.0);
        assert_eq!(monitors[0].width, 1920);
        assert!(monitors[0].focused);
        assert!(!monitors[1].focused);

        for scale in ["0", "-2", "17", "1e39", "\"two\"", "null"] {
            let raw = format!(r#"[{{"name":"DP-1","width":800,"height":600,"scale":{scale}}}]"#);
            let err = parse_monitors(raw.as_bytes()).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidGeometry, "scale {scale}");
        }
        let err = parse_monitors(br#"[{"scale":1,"width":10,"height":10}]"#).unwrap_err();
        assert_eq!(
            err.code(),
            ErrorCode::CaptureFailed,
            "a monitor needs a name"
        );
        let err = parse_monitors(b"[]").unwrap_err();
        assert_eq!(err.code(), ErrorCode::NoMonitor);
        let err = parse_monitors(b"{}").unwrap_err();
        assert_eq!(err.code(), ErrorCode::CaptureFailed);
    }

    #[test]
    fn target_parsing() {
        assert_eq!(Target::parse("window").unwrap(), Target::Window);
        assert_eq!(Target::parse("screen").unwrap(), Target::Screen);
        let err = Target::parse("both").unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidParams);
        assert!(err.message().contains("window"));
    }

    #[test]
    fn png_dimensions_come_from_the_header() {
        let bytes = png(1600, 1200);
        assert_eq!(png_dimensions(&bytes).unwrap(), (1600, 1200));
        let err = png_dimensions(b"GIF89a").unwrap_err();
        assert_eq!(err.code(), ErrorCode::CaptureFailed);
        let err = png_dimensions(&png(0, 10)[..20]).unwrap_err();
        assert_eq!(err.code(), ErrorCode::CaptureFailed);
        let mut bad = png(10, 10);
        bad[12] = b'X';
        assert!(png_dimensions(&bad).is_err());
    }

    #[test]
    fn magick_dimension_text() {
        assert_eq!(parse_dimensions_text("1280x752"), Some((1280, 752)));
        assert_eq!(parse_dimensions_text(" 1280x752\n"), Some((1280, 752)));
        assert_eq!(parse_dimensions_text("0x752"), None);
        assert_eq!(parse_dimensions_text("1280"), None);
        assert_eq!(parse_dimensions_text("axb"), None);
    }

    #[test]
    fn rect_validation() {
        assert!(validate_rect(0, 0, 1, 1).is_ok());
        assert!(validate_rect(-8000, -8000, 100, 100).is_ok());
        assert_eq!(
            validate_rect(0, 0, 0, 10).unwrap_err().code(),
            ErrorCode::InvalidGeometry
        );
        assert_eq!(
            validate_rect(0, 0, MAX_DIMENSION + 1, 10)
                .unwrap_err()
                .code(),
            ErrorCode::InvalidGeometry
        );
    }
}
