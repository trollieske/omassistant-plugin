//! The capability registry. This list is the single source of truth for `capabilities`, for
//! `consent.grant` validation and for the audit log's `capability` field.
//!
//! Every capability is a separate grant: a person who allowed a screenshot has not allowed a key
//! press, and a person who allowed typing has not allowed the clipboard.

/// One thing this daemon can do on the person's behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capability {
    /// The wire name, e.g. `observe.screen`.
    pub name: &'static str,
    /// One sentence for a person.
    pub description: &'static str,
}

/// `observe.screen`: capture the focused window or the focused monitor, once, on request.
pub const OBSERVE_SCREEN: Capability = Capability {
    name: "observe.screen",
    description: "Capture the focused window or screen once",
};

/// `act.type`: type text into the focused window. Typed text is its own grant:
/// it is the one action whose exact content the card can show, so each call is
/// confirmed on its own.
pub const ACT_TYPE: Capability = Capability {
    name: "act.type",
    description: "Type text into the focused window",
};

/// `act.keys`: press one validated key chord in the focused window. A key
/// approval lasts 30 s in that same window, so navigation (Tab, arrows, Enter)
/// does not prompt for every press.
pub const ACT_KEYS: Capability = Capability {
    name: "act.keys",
    description: "Press one validated key chord in the focused window",
};

/// `act.pointer`: move the real pointer inside the focused window and click or
/// scroll. It is its own grant because a click can press anything on screen.
pub const ACT_POINTER: Capability = Capability {
    name: "act.pointer",
    description: "Move the pointer and click or scroll in the focused window",
};

/// `act.clipboard`: read or write the clipboard. Reading is as sensitive as writing, so both
/// directions share one grant and one audit trail.
pub const ACT_CLIPBOARD: Capability = Capability {
    name: "act.clipboard",
    description: "Read or write the clipboard (reading is as sensitive as writing)",
};

/// `act.launch`: start one of a fixed set of desktop apps. No arguments, no paths, no shell.
pub const ACT_LAUNCH: Capability = Capability {
    name: "act.launch",
    description: "Launch a fixed desktop app (browser, terminal, editor, mail)",
};

/// `shell.run`: run one of a fixed set of desktop actions.
pub const SHELL_RUN: Capability = Capability {
    name: "shell.run",
    description: "Run a named desktop action (lock, volume, brightness, workspace, layout)",
};

/// `shell.exec`: run one command line in the person's own shell. This is the
/// capability that makes the assistant as capable as the person at their own
/// terminal — creating folders, moving files, running git — instead of only the
/// handful of actions in the fixed tables. It is deny-by-default and every call
/// is confirmed with the exact command on screen unless the person granted it for
/// a while.
pub const SHELL_EXEC: Capability = Capability {
    name: "shell.exec",
    description: "Run one command in your shell (with your permissions, never root)",
};

/// Everything this daemon implements. The browser and mail are not here.
pub const CAPABILITIES: &[Capability] = &[
    OBSERVE_SCREEN,
    ACT_TYPE,
    ACT_KEYS,
    ACT_POINTER,
    ACT_CLIPBOARD,
    ACT_LAUNCH,
    SHELL_RUN,
    SHELL_EXEC,
];

/// Look a capability up by its wire name.
pub fn find(name: &str) -> Option<&'static Capability> {
    CAPABILITIES
        .iter()
        .find(|capability| capability.name == name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_is_the_m2_capabilities() {
        assert_eq!(CAPABILITIES.len(), 8);
        let capability = find("observe.screen").unwrap();
        assert_eq!(capability.name, "observe.screen");
        assert_eq!(
            capability.description,
            "Capture the focused window or screen once"
        );
        let capability = find("act.type").unwrap();
        assert_eq!(capability.description, "Type text into the focused window");
        let capability = find("act.keys").unwrap();
        assert_eq!(
            capability.description,
            "Press one validated key chord in the focused window"
        );
        let capability = find("act.pointer").unwrap();
        assert_eq!(
            capability.description,
            "Move the pointer and click or scroll in the focused window"
        );
        let capability = find("act.clipboard").unwrap();
        assert_eq!(
            capability.description,
            "Read or write the clipboard (reading is as sensitive as writing)"
        );
        let capability = find("act.launch").unwrap();
        assert_eq!(
            capability.description,
            "Launch a fixed desktop app (browser, terminal, editor, mail)"
        );
        let capability = find("shell.run").unwrap();
        assert_eq!(
            capability.description,
            "Run a named desktop action (lock, volume, brightness, workspace, layout)"
        );

        assert!(find("").is_none());
    }

    #[test]
    fn names_are_unique_and_dotted() {
        let mut names: Vec<&str> = CAPABILITIES.iter().map(|cap| cap.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "two capabilities share a name");
        for capability in CAPABILITIES {
            assert!(
                capability.name.contains('.'),
                "{} should be area.method",
                capability.name
            );
            assert!(!capability.description.is_empty());
        }
    }
}
