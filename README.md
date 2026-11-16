# Omassistant

A permissioned, screen-aware assistant for the Omarchy desktop. One overlay for the whole desktop:
summon it with a hotkey, ask about what is on screen, and let it act only when you grant the
capability. The permission model is the product, not a dialog bolted on the side.

![Omassistant's sidebar beside the desktop](preview.png)

Omassistant is built for Omarchy Quattro (Hyprland + Quickshell). It is a thin, keyboard-first QML
overlay over [pi](https://pi.dev), with a small Rust broker in between. The broker owns consent,
expiry and the audit log; the model never does. The overlay can look at a window, type, press keys,
click, use the clipboard, launch one of a few named apps and run a fixed set of desktop actions —
each as its own grant, each audited, all revocable.

> **The honest version:** this is not a sandbox. Plugins run as arbitrary code inside
> `omarchy-shell`, and Omassistant can type and click as you when you allow it. The broker makes
> those powers explicit, separate and temporary; it does not make unknown code safe. Install only
> from a source you trust and read the grants before you press ALLOW.

## What it does

- **Look at the current window.** `Ctrl+O` or `/look` captures the focused window or monitor after
  a visible consent prompt. The card blanks while the shot is taken so the overlay is not in it.
- **Act, with a grant for each kind of action.** Type, press keys, click and scroll, read/write the
  clipboard, launch an app, or run a named desktop action. `shell.run` is a fixed table (lock,
  volume, brightness, workspace, layout, fullscreen/maximize/restore); there is no arbitrary shell
  and no caller-supplied program. Looking at the screen is `observe.screen`, not a screenshot
  action: the capture is brokered and audited instead of opening a picker.
- **Remember the conversation.** Each conversation is a pi session under
  `~/.local/state/omassistant/pi-sessions/`. The newest session is resumed on shell start; `Ctrl+H`
  switches between them; the transcript is rebuilt from pi's own messages.
- **Dictate.** `Ctrl+D` asks Omarchy for one dictation, so it uses the backend you have set with
  `omarchy default dictation` — voxtype, Superwhisper or Hex. The card mirrors that backend's own
  state, so "listening…" is real rather than a guess.
- **Pick a model without restarting.** Models and keys belong to pi, not Omassistant. The Settings
  sheet lists what pi can already use; setting a model is an RPC call, so the conversation survives.
  Optional automatic routing sends short information questions to a Fast model and longer,
  creative or action-shaped work to a Best model. Images only go to a model that declares image
  input. Fast falls back to a local model and then to the default — never to a random "free" cloud
  model.
- **See what it cost.** The COSTS sheet sums provider-reported `usage.cost.total` from pi's own
  session records, per conversation and per model. Missing usage is shown as $0; it is not an
  estimate.
- **Live in the shell.** A right sidebar that stays out of the way, a bar widget, and a global
  STOP key. A desktop companion card can be turned on in settings; it is off by default, so a new
  install adds nothing to the desktop but the bar diamond. The card releases keyboard focus while
  an action runs so the target app gets the keys, but the sidebar and STOP stay visible.

## How it works

```text
                  hotkey / bar widget
                          │
                          ▼
                Omassistant.qml (QML) ──── RPC ────▶ pi (agent + model)
                          │                          │
                          │                          │ MCP
                          │                          ▼
                          └── Unix socket ──▶ omassistantd ◀── omassistant-mcp
                                                 │
                                        grants.json + audit/
```

- **`Omassistant.qml` and its siblings** are the overlay, composer, permissions sheet, settings,
  costs view, companion and bar widget. They draw with the shell's own `qs` components.
- **`omassistantd`** is a user-level Rust broker. It checks grants, enforces expiry, runs the fixed
  tools and writes the audit. It has no network listener; the socket lives in
  `$XDG_RUNTIME_DIR/omassistant/` with mode 0600.
- **`omassistant-mcp`** is the MCP server pi talks to. Every tool call becomes one broker request;
  the broker decides whether it is allowed, and the card asks when a fresh confirmation is needed.
- **pi** is the agent and the model runtime. It owns sessions, credentials, providers and model
  usage. Omassistant starts it with `--mode rpc` in a workspace of its own, never in a terminal.

Observed content is untrusted data. A page, a mail or a window title can say "ignore your
instructions"; the broker still checks the grant. The model can ask; it cannot grant.

## Requirements

- **Omarchy Quattro** with Hyprland and Quickshell. Wayland only; X11 is not supported.
- **Rust 1.85 or newer** to build the broker (edition 2024). The build is local and user-level;
  nothing is installed system-wide. If you prefer a package, the same source is in `daemon/`.
- **pi** with at least one provider signed in. On Omarchy Quattro, pi is part of the managed mise
  tool set and installs itself on first use; run `pi --version` once. If you manage tools another
  way, install pi from [pi.dev](https://pi.dev), then run `pi` once. Omassistant does not ship a
  model and never stores a provider key. If pi cannot start, the overlay shows a setup card with
  the command, a link to pi.dev and a Retry button; it never installs an agent silently.
- **Desktop tools** on `PATH`: `grim` (capture), `hyprctl` (Hyprland), `wtype` (typing and keys),
  `wl-copy` and `wl-paste` (clipboard), `gtk-launch` and `xdg-mime` (opening your apps),
  `curl` (loopback local-model discovery), and the usual Omarchy helpers
  (`omarchy-launch-*`, `omarchy-audio-output-volume`, `omarchy-brightness-display`).
- **Optional:** ImageMagick (`magick`) for smaller screenshots; `wlrctl` for pointer control;
  one of the dictation backends Omarchy offers (voxtype, Superwhisper, Hex); any mail app the
  desktop can open for `mailto:` links (see below).
- A free keybinding for the overlay. The plugin does not register global shortcuts itself.

## Install

### 1. Add the plugin

```bash
omarchy plugin add https://github.com/trollieske/omassistant-plugin.git --enable
```

The manifest id is `omassistant`, so the plugin is cloned into
`~/.config/omarchy/plugins/omassistant`. The repository is also the source for the broker; the
`daemon/` directory is included in the clone.

### 2. Build and install the broker

```bash
cd ~/.config/omarchy/plugins/omassistant
./daemon/install-user.sh
```

This runs `cargo build --release` and installs three binaries into `~/.local/bin`:

- `omassistantd` — the broker
- `omassistantctl` — the CLI (`ping`, `consent`, `costs`)
- `omassistant-mcp` — the MCP server pi starts

It creates no system service and needs no root helper. Make sure `~/.local/bin` is on the `PATH`
that `omarchy-shell` sees. The overlay starts the broker on demand; it dies with the shell.

### 3. Install the desktop pieces

```bash
omarchy pkg add grim wtype wl-clipboard curl
```

Optional extras:

```bash
omarchy pkg add imagemagick        # smaller JPEG captures
omarchy pkg aur add wlrctl         # pointer move/click/scroll
```

`act.launch` names four app *kinds* — browser, terminal, editor, mail — never a program. The app
behind each is yours: browser, terminal and editor use Omarchy's own launchers, so they follow
`omarchy default browser|terminal|editor`. Mail uses whatever this desktop opens for `mailto:`
links (`xdg-mime query default x-scheme-handler/mailto`), and you can point it at a specific app
with a desktop id instead:

```json
{ "launch": { "mail": "io.github.billsecond.mailcraft.desktop" } }
```

That is `~/.config/omassistant/config.json`; the id is the name of the app's `.desktop` file
(`ls /usr/share/applications ~/.local/share/applications`). It is an app id, not a command — the
broker refuses anything else, so neither the model nor a stray socket client can pick the program.

Set up dictation with Omarchy's own flow (`omarchy install dictation voxtype`, or Superwhisper)
if you want `Ctrl+D`. Omassistant reads whichever backend `omarchy default dictation` names and
never bundles, downloads or configures a model itself.

Set up **pi** with at least one provider. On Omarchy Quattro, run `pi --version` once: the managed
mise configuration installs it lazily and keeps it updated. If you use another setup, install pi
from [pi.dev](https://pi.dev) and run `pi` once. Any provider or local model that pi can use will
appear in Omassistant's Settings sheet.

The overlay is honest when pi is missing: it opens a **setup card** with the `pi --version`
command, a link to pi.dev, a copy button and **Retry**. It does not run a network installer for
you. A first start can be slow while mise fetches pi; the card says so instead of showing a fault.

### 4. Bind the keys and reload the shell

Add this to `~/.config/hypr/hyprland.lua` (choose keys that are free on your keyboard):

```lua
hl.layer_rule({ match = { namespace = "omassistant" }, no_anim = true, animation = "none" })

o.bind("SUPER + ALT + A", "Omassistant", { panel = "omassistant" })
o.bind("SUPER + ALT + ESCAPE", "Omassistant: stop", "omarchy-shell omassistant stop")
o.bind("SUPER + SHIFT + ESCAPE", "Omassistant: revoke everything",
  "omarchy-shell omassistant lockdown >/dev/null 2>&1; omassistantctl consent revoke-all >/dev/null 2>&1")
```

Then reload the shell:

```bash
omarchy-restart-shell
```

The `--enable` above places the bar widget. You can move it in the bar settings, or open the card
with `omarchy-shell shell toggle omassistant` and `omarchy-shell omassistant ask "..."`.

### 5. First run

Open the overlay (or click the bar diamond). The card starts pi and shows the model picker. Choose a
model in **Settings** (gear or `Ctrl+,`), or leave pi's saved default. Then ask something simple:
*"Summarise what is open on my screen."* The first look asks for `observe.screen` in plain words.
Allow once, and the card steps aside for the capture.

Open the permissions sheet with the pill at the bottom of the card, `Ctrl+P`, or `/permissions`.
Every capability is separate; grants can be time-limited; the audit is immediate.

## Using it

| Key / command | What it does |
|---|---|
| `SUPER+ALT+A` | Open or close the sidebar (your binding) |
| `Enter` | Send |
| `Esc` | Close the card |
| `Ctrl+O` | Look at the focused window, then ask |
| `/look why…` | Look and ask in one line |
| `Ctrl+D` | Start/stop local dictation |
| `Ctrl+H` | Conversation history |
| `Ctrl+K` | New conversation |
| `Ctrl+,` | Settings |
| `Ctrl+P` | Permissions |
| `Ctrl+C` | Stop a streaming answer |
| `SUPER+ALT+ESC` | Stop the current answer and deny pending approvals |
| `SUPER+SHIFT+ESC` | Revoke every grant (lockdown) |
| `/help` | The full in-card command list |
| `/clear`, `/stop`, `/settings`, `/permissions`, `/costs` | The obvious shortcuts |
| `/run <action>` | Run a fixed `shell.run` action |
| `/open browser\|terminal\|editor\|mail` | Open the app you have for that kind |

From outside the card:

```bash
omarchy-shell omassistant ask "what is on my screen?"
omarchy-shell omassistant status
omarchy-shell omassistant stop
omarchy-shell omassistant permissions
omarchy-shell omassistant costs
omassistantctl consent revoke-all
omassistantctl costs --json
```

`omassistantctl` works without the overlay, which is why the lockdown key can always revoke.

## Permissions

Seven capabilities, each a separate grant:

- `observe.screen` — capture the focused window or a monitor and send the image to the selected model
- `act.type` — type text into the focused app
- `act.keys` — press keys and shortcuts in the focused window
- `act.pointer` — click, double-click, right-click, middle-click, scroll or move inside the focused window
- `act.clipboard` — read or write the clipboard
- `act.launch` — open your browser, terminal, editor or mail app, and bring its window to the
  front (restoring it if it was minimized)
- `shell.run` — a fixed table of desktop actions (lock, volume, brightness, workspace, layout,
  fullscreen/maximize/restore)

How it behaves:

- **The first call asks.** The consent names the exact effect and the app/window it applies to.
  Nothing runs before the broker sees an allow.
- **Keys are window-bound.** A key approval covers the focused window for 60 seconds. Typing is its
  own grant; a key approval never implies typing. Pointer, clipboard, launch and capture are one
  call per confirmation unless you grant a standing grant from the sheet.
- **Standing grants expire.** The sheet offers 5, 10, 15, 20, 25, 30 or 60 minutes, per capability,
  plus ALLOW EVERYTHING and REVOKE EVERYTHING. A countdown is visible while a grant is live.
- **An approval names a window, and that window is checked again.** Between the question and your
  answer focus can move: the card does not hold it. So the broker reads the focused window again
  immediately before a capture, a keystroke, some typing or a click, and compares the app class and
  the window's compositor address with what you approved. If they differ, nothing is captured,
  typed or pressed; the call fails with `focus_moved` and the audit line says which window was
  approved. A window that only moved or resized is still the same window, and is captured where it
  is now.
- **The broker re-checks every call.** A changed window, an expired grant or a stopped run fails
  closed. The model cannot widen a grant, and no observed text is treated as an instruction.
- **The card steps aside, not away.** While an action runs, the sidebar releases keyboard focus so
  the target app receives the keys; it stays visible with STOP. During a capture it blanks so it is
  not in the shot.
- **Every call is audited.** `~/.local/state/omassistant/audit/` gets one JSONL line per capability
  and consent call: method, capability, decision, grant mode, target app class and outcome. Window
  titles, prompts, answers, typed text, clipboard bodies and image data are not written there.
- **STOP and lockdown.** STOP aborts the answer, denies pending confirmations and ends the run's
  grants. The lockdown binding drops every grant and fails waiting confirmations. Neither needs the
  card to be focused.

The broker is a permission gate, not a sandbox. It cannot make a malicious plugin safe, and it
cannot stop a model from being wrong. It can make every privileged effect visible, separate and
revocable.

## Models, routing and cost

Omassistant does not ship, proxy or resell a model. It drives pi, and pi drives the provider you
already configured. Keys live in pi's credential store; Omassistant has no key field, never reads
the credential store and refuses `--api-key` on the command line because argv is readable.

The Settings sheet has one model choice per role:

- **Default** — every question when automatic choice is off.
- **Fast** — short, informational questions.
- **Best** — longer, creative or action-shaped work.
- **Local** — the explicit local model used by *Keep everything on this computer*.

Automatic choice is off by default and becomes meaningful only when Fast and Best are different.
A switch is a single `set_model` RPC call: no restart, no lost session. The routing decision is a
pure function in `Routing.js`, so it can be read and tested without the model. It routes short
questions to Fast, complex or action-shaped requests to Best, keeps follow-ups on the previous tier,
sends images only to a vision model, and blocks an image or a cloud request when local-only is on.
That block is checked again at the moment a question is sent, not only when the route is chosen: if
a switch to the local model fails or times out, the question and its screenshot are **not** sent on
the model that happens to be loaded. The card says so, keeps your draft, and waits for you.

Local discovery probes only loopback addresses: Ollama on `127.0.0.1:11434`, LM Studio on
`127.0.0.1:1234`, llama.cpp on `127.0.0.1:8080`. If you choose *Find local models*, Omassistant
merges `omassistant-*` providers into pi's own `models.json` with a backup and mode 0600, then
restarts pi once. It uses placeholder keys for those local providers; no real key is stored there.

The COSTS sheet is computed from pi's session records: it sums `usage.cost.total` per message and
per model, and reports what the provider recorded. Missing usage is $0, not an estimate, and the
report never reads the prompt or answer text. Use it as a provider-billed view, not a budget.

## Privacy and files

Omassistant has no telemetry and no network server. The only network calls are pi's own model
requests and the loopback discovery probes above. A screen capture is sent to the model you chose;
with a local model it stays on the machine. The broker's audit and runtime snapshot contain no
content — but pi's session file does, because that is how the conversation is remembered.

| Path | Contents | Notes |
|---|---|---|
| `~/.config/omassistant/config.json` | Your settings (model roles, flags, workspace) | No keys |
| `~/.local/state/omassistant/audit/` | One JSONL line per broker call | No titles, prompts, text or images |
| `~/.local/state/omassistant/grants.json` | The grant ledger | 0600 |
| `~/.local/state/omassistant/pi-sessions/` | pi conversations, screenshots and tool arguments | 0700; delete to erase the memory |
| `~/.local/state/omassistant/sessions.json` | The history picker index (titles, timestamps, turns) | No full transcript |
| `~/.local/state/omassistant/shots/` | The full PNG for a look | 0600; treat as sensitive |
| `~/.local/state/omassistant/workspace/.pi/mcp.json` | The one MCP server the agent may use | In the scratch workspace |
| `$XDG_RUNTIME_DIR/omassistant/state.json` | The bar's snapshot (live/busy/waiting/unread/fault/dictation) | No content |
| `~/.config/omarchy/plugins/omassistant/` | The plugin and the broker source | Git-managed |
| `~/.local/bin/omassistant*` | The three broker binaries | User-level |
| `~/.pi/agent/models.json` | pi's providers; only changed if you add local models | Backup `.omassistant.bak`; no real keys added |

If you want the strongest privacy, use a local model, grant only one look at a time, and remove
`pi-sessions/`, `shots/` and `audit/` when you are done. The card's `/clear` starts a new
conversation; it does not delete the old session files.

## Updating

```bash
omarchy plugin update omassistant
cd ~/.config/omarchy/plugins/omassistant
./daemon/install-user.sh
```

The plugin update replaces the clone (including the broker source). Rebuilding the three binaries
after an update keeps the broker in step with the QML. Your settings, sessions, grants and audit
are separate and survive.

## Removing

```bash
omarchy plugin disable omassistant
omarchy plugin remove omassistant
rm -f ~/.local/bin/omassistantd ~/.local/bin/omassistantctl ~/.local/bin/omassistant-mcp
```

The last line is optional cleanup for an install that is not managed by the package manager.
To delete its data as well:

```bash
rm -rf ~/.local/state/omassistant ~/.config/omassistant
```

That deletes the conversation history, screenshots, grants and audit. If you added local models,
remove the `omassistant-*` providers from `~/.pi/agent/models.json` (a `.omassistant.bak` backup is
next to it). Remove the Omassistant lines from `~/.config/hypr/hyprland.lua` and run
`omarchy-restart-shell`. The dictation files, if you set a backend up, are Omarchy's and are left
alone.

## Troubleshooting

**"The broker is not reachable."** Check `command -v omassistantd omassistantctl omassistant-mcp`
and `omassistantctl ping --json`. If you just built the broker, make sure `~/.local/bin` is on the
`PATH` that the shell was started with, then `omarchy-restart-shell`. The plugin starts the broker
on demand; there is no service to enable.

**The overlay opens without pi.** The card should show its setup state rather than a bare OFFLINE
error. The Omarchy route is `pi --version` once (mise installs the managed tool); otherwise install
pi from pi.dev. Then sign in by running `pi` once in a terminal. The card starts pi with
`--mode rpc` in its scratch workspace; a pi that needs an interactive login will not work until
you have done that. The Settings sheet only lists what pi reports.

**A look is black or shows the wrong window.** The card blanks and steps aside during a capture;
the capture is the focused window (or the focused monitor). Focus the window first, then use
`Ctrl+O` or `/look`. If the window is on another monitor, use the monitor target. Permissions must
still be live. If focus moved while you were answering the prompt, the call fails with
`focus_moved` instead of capturing the window that replaced it: look again, in the window you meant.

**Keys go to the card instead of the app.** The card should release keyboard focus while a granted
action runs. If a standing key grant is live and the card is focused, click the app window first or
press `Esc` and reopen. A key approval is bound to one window and 60 seconds; it cannot follow you
to another app, and if focus moved before the keys went out the call fails with `focus_moved` and
nothing is pressed.

**Pointer does nothing.** Pointer control needs `wlrctl` (Wayland virtual pointer). If it is not
installed, the permission is offered but calls fail closed. Install `wlrctl` from the AUR, restart
the shell and try again.

**Costs show $0.** pi records cost only when the provider returns usage. Some providers and local
models omit it; the sheet reports what is in the session file, not a guess. Check the provider's own
dashboard for billing.

**A model switch did not happen.** Automatic choice is off until you set both tiers. The current
conversation stays on its model when it is already long; explicit depth or an image can force Best.
Open Settings to see the resolved roles and the actual current model.

**Dictation does nothing.** `Ctrl+D` runs `omarchy-dictation toggle`, so it needs a backend chosen
with `omarchy default dictation` and that backend installed. Check the phrase with
`omarchy dictation toggle` in a terminal; if that works, so does the card.

**The QML fails to load after an update.** Validate the clone with
`omarchy plugin validate ~/.config/omarchy/plugins/omassistant`, then `omarchy-restart-shell`. The
plugin uses the shell's own `qs` modules; a Quattro update can change them.

## Development

The repository is a plugin repository: `manifest.json` and the QML entry points are at the root.
The broker is a self-contained Rust crate in `daemon/` with its own `Cargo.lock`. Useful commands:

```bash
omarchy plugin validate .
cd daemon && cargo test && cargo clippy --all-targets -- -D warnings
```

The plugin is deliberately thin. Its permission-relevant decisions live in the broker; the model
routing is a pure file (`Routing.js`) and the QML panels are lazy-loaded so a broken settings sheet
cannot take the conversation down.

## License

MIT OR Apache-2.0. See [LICENSE](LICENSE), [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).

Omassistant is an independent community project. It is not an official Omarchy component, and the
plugin marketplace listing is not a security audit or an endorsement.
