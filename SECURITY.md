# Security notes for reviewers

Omassistant is a desktop assistant for Omarchy. It is two things in one repository:

- a **QML plugin** at the repository root (overlay, bar widget, settings), running inside
  `omarchy-shell`;
- **`omassistantd`**, a user-level Rust broker under `daemon/`, plus two small binaries
  (`omassistantctl`, `omassistant-mcp`).

There is **no service, no system package, no root helper and no network listener**. Three binaries are
installed into `~/.local/bin` by `daemon/install-user.sh`, which never calls `sudo`. Users who prefer a
package install the same source through the OPR recipe.

This document is a map: what the assistant can do, the invariants that hold it, the test that holds
each one, and what is deliberately out of scope. Every claim below has a file and a test name you can
run.

## The shape of it

```
Hyprland ──> omarchy-shell (QML plugin) ──RPC──> pi (agent + model, the person's own)
                      │                            │
                      │                            │ MCP (one server, declared tools only)
                      └──── Unix socket ────> omassistantd <──── omassistant-mcp
                                                 │
                                        grants.json + audit/
```

`pi` is the person's own agent runtime and provider credentials; this repository never reads or stores
an API key for a model. The broker owns the only path from the model to the desktop, and it writes the
audit.

The socket lives in `$XDG_RUNTIME_DIR/omassistantd.sock`; the state tree is `0700` and the socket is
created `0600` (`the_state_tree_and_the_socket_are_private`).

## What it can do, and what it refuses

| Capability | Can | Refuses |
|---|---|---|
| `observe.screen` | capture the focused window or one monitor | acting if focus moved between the question and the answer |
| `act.type` | type text into the focused window | control characters other than `\n`/`\t`; no window; focus moved |
| `act.keys` | press one validated chord | chords that are compositor shortcuts, malformed chords, focus moved |
| `act.pointer` | click/scroll/move at a fraction of the window | points outside the window; focus moved |
| `act.clipboard` | read or write the clipboard | text over the cap; a missing `wl-copy`/`wl-paste` |
| `act.launch` | fixed kinds (browser/terminal/editor/mail) or any installed desktop id | any caller string reaching an exec; ids that are not plain file names |
| `shell.run` | a fixed table of desktop actions | anything not in the table |
| `shell.exec` | one command line in the person's own shell | `sudo`, `doas`, `pkexec`, `su` (by name); multi-line input; cwd outside the person's directories |
| `web.search` | search an index and read one public page | non-`http(s)` URLs; loopback, RFC 1918, link-local, `.local`, metadata addresses |

## Invariants

**1. The model's only tools are the broker's.** `pi` is started with
`--no-extensions --no-skills --no-prompt-templates --no-context-files -e builtin:mcp
--tools mcp__omassistant__* --no-builtin-tools` (`Omassistant.qml`, `baseArgv()`). Without
`--no-builtin-tools` pi would offer its own `bash` tool, which would bypass the broker entirely — that
flag is not optional. `scripts/check.sh` fails if either the allowlist or the flag disappears.

**2. Nothing acts without a live grant.** No grant means `consent_required`, and the refusal is
audited with the capability it would have used. Tests:
`acting_without_consent_is_denied_and_audited_with_the_right_capability`,
`observe_screen_without_consent_is_denied_before_anything_runs`,
`a_denied_confirmation_does_nothing_and_is_audited`.

**3. An approval names one window, not one app.** The approved window is identified by its app class
**and** the compositor's address (`foot#0x1`). It is read again immediately before typing, pressing,
clicking or capturing, and compared; a mismatch fails with `focus_moved` and does nothing. A
60-second key approval is bound to that same pair, so a second terminal is a new confirmation, and a
reused approval is checked against the window it named — not against whatever has focus by then.
Tests: `a_focus_change_during_the_confirmation_types_nothing`,
`a_focus_change_during_the_confirmation_presses_nothing`,
`a_capture_reads_the_window_again_after_the_answer`,
`a_key_approval_follows_only_its_own_window` (same class, different address),
`a_second_window_of_the_same_app_is_not_the_approved_one`,
`an_approval_rebuilt_from_its_scope_still_names_that_window`,
`a_run_grant_is_bound_to_one_window_not_one_app`.

**4. Consent can be taken back mid-flight.** A run epoch is bumped by `STOP`,
`consent.revoke_run` and `lockdown`; an approval that crosses a bump cannot mint a grant, so a
question waiting on the card cannot be answered into an action after the person stopped. Tests:
`revoke_all_is_one_audited_kill_switch`, `one_once_grant_is_reported_to_exactly_one_of_two_racing_calls`.

**5. There is no root.** `shell.exec` refuses `sudo`, `doas`, `pkexec` and `su` by the program name
before anything runs, and says why (`one_command_runs_in_the_persons_shell`). Nothing in the installer
escalates; the package installs user-level binaries only.

**6. Network egress is a capability, not a side effect.** `web.search` and `web.fetch` are consented
like everything else. URLs must be `http`/`https` and the host must be public: `localhost`, `127/8`,
`::1`, RFC 1918, link-local, `.local`, `.internal` and a URL with credentials are refused by name, and
a name that resolves into a private range is refused after resolution
(`only_public_http_urls_are_fetched`). The audit records the host and the length, never the query.

**7. The audit holds no content.** One JSONL line per call with method, outcome, capability, grant
kind, window class, geometry, byte counts, key counts and error code. Never a window title, prompt,
answer, typed text, clipboard content, image or search query
(`the_audit_log_never_holds_a_title_or_image_data`).

**8. Secrets stay in one private file.** Optional search keys live in
`$XDG_STATE_HOME/omassistant/web.json` with mode `0600`, are written through stdin (never argv, never a
shell), are never read back to the card, and never appear in the audit or the repository.
`scripts/check.sh` scans the tree for key shapes, and the staging script refuses a credential-like
string; both are in the release gate.

**9. Local-only is enforced when the question is sent**, not when the route is chosen: a failed or
timed-out switch to the local model refuses to send, rather than falling back to a cloud model
(`local-only refuses to send on a cloud model after a failed switch`).

**10. Trust mode is off unless the person turns it on.** `omassistantctl trust on` writes
`trust.json` in the state directory; the broker then keeps standing grants, every call is still
audited with `grant=standing`, the card's pill reads TRUSTED, and `trust off` revokes exactly the
grants it minted — not the ones the person made by hand (`trust_off_takes_back_what_trust_gave`).

**11. Pictures are files with private modes.** A capture is written `0600` and the audit keeps the
path and the byte count. The look preview shown before approving a capture is `0600` and deleted when
the confirmation is resolved.

## What this does not protect you from

Stated plainly, because a reviewer should not have to infer it:

- **Your own provider.** What the model does with a prompt or a screenshot afterwards is between you
  and your provider; the broker's promise ends at the model it handed data to.
- **Prompt injection.** Text from a page, a window or a file is data, and a model can be talked into
  proposing something foolish. The mitigation is structural, not clever: the card names the exact
  command or the exact effect before it runs, and the audit records what did run. Nothing is
  pre-approved unless you turned trust mode on.
- **A hostile compositor.** The broker believes `hyprctl` about which window has focus. A compromised
  compositor can lie to any program on the same seat.
- **Sandboxing.** Actions run with your own permissions, by design — it is your desktop and your
  files. The capability list limits *what the model may ask for*, not what a command can do once you
  approve it. That is why `shell.exec` shows the line first.
- **The install script's own inputs.** It builds from the repository it sits in and installs to
  `~/.local/bin`; it does not fetch anything else.

## Verifying it yourself

```sh
mise exec rust -- ./scripts/check.sh          # fmt, clippy, 137 lib + 45 socket tests, QML smoke,
                                             # staging guards, key-shape scan
mise exec rust -- cargo test -p omassistantd  # or the same tests directly
node shell/plugins/omassistant/test/routing.test.js
omassistantctl capabilities --json           # what is granted, right now
omassistantctl audit --lines 50              # what has run, without content
omassistantctl trust status                  # whether anything asks on this machine
```

The socket-level tests live in `daemon/tests/protocol.rs`; they drive a real daemon over its socket
with a fake `hyprctl`/`wtype`/`grim`, so the consent flow, the focus re-check and the audit are
exercised end to end rather than mocked at the function boundary.

## Reporting a problem

Open a private security advisory on `trollieske/omassistant-plugin` (Security → Report a
vulnerability). Please do not open a public issue for something that changes how a consent decision
can be bypassed.
