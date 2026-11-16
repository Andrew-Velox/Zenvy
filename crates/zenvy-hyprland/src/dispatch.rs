//! Turning typed [`DesktopCommand`]s into Hyprland dispatch strings.
//!
//! Hyprland 0.56 evaluates `dispatch` as **Lua** (`hl.dsp.*`); older releases use the legacy
//! `dispatch <name> <args>` form (see PLAN.md Appendix A, finding 1c/1d). Both builders only ever
//! interpolate values that were validated by `zenvy-proto` types (integers and `0x<hex>`
//! addresses), never client-supplied text.

use zenvy_proto::DesktopCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    Lua,
    Legacy,
}

pub fn lua(cmd: &DesktopCommand) -> String {
    match cmd {
        DesktopCommand::SwitchWorkspace { id } => format!("hl.dsp.focus({{ workspace = {id} }})"),
        DesktopCommand::FocusWindow { address } => format!(
            "hl.dsp.focus({{ window = \"address:{}\" }})",
            address.as_str()
        ),
        DesktopCommand::CloseWindow { address } => format!(
            "hl.dsp.window.close({{ window = \"address:{}\" }})",
            address.as_str()
        ),
        DesktopCommand::ToggleFloating { address } => {
            format!(
                "hl.dsp.window.float({{ action = \"toggle\", window = \"address:{}\" }})",
                address.as_str()
            )
        }
        DesktopCommand::MoveWindowToWorkspace { address, workspace } => {
            format!("hl.dsp.window.move({{ workspace = \"{workspace}\", follow = false, window = \"address:{}\" }})", address.as_str())
        }
    }
}

pub fn legacy(cmd: &DesktopCommand) -> String {
    match cmd {
        DesktopCommand::SwitchWorkspace { id } => format!("workspace {id}"),
        DesktopCommand::FocusWindow { address } => {
            format!("focuswindow address:{}", address.as_str())
        }
        DesktopCommand::CloseWindow { address } => {
            format!("closewindow address:{}", address.as_str())
        }
        DesktopCommand::ToggleFloating { address } => {
            format!("togglefloating address:{}", address.as_str())
        }
        DesktopCommand::MoveWindowToWorkspace { address, workspace } => {
            format!(
                "movetoworkspacesilent {workspace},address:{}",
                address.as_str()
            )
        }
    }
}

/// Full socket request for a command under the given mode.
pub fn request(mode: DispatchMode, cmd: &DesktopCommand) -> String {
    match mode {
        DispatchMode::Lua => format!("dispatch {}", lua(cmd)),
        DispatchMode::Legacy => format!("dispatch {}", legacy(cmd)),
    }
}

/// What the compositor said to a deliberately invalid, non-mutating `dispatch`.
/// Lua mode complains about `hl.dispatch(...)`; legacy mode says "Invalid dispatcher" [A: the
/// legacy wording is from memory and still needs a fixture from an older Hyprland].
pub fn classify_dispatch_reply(reply: &str) -> Option<DispatchMode> {
    let r = reply.to_ascii_lowercase();
    if r.contains("hl.dispatch") || r.contains("lua") {
        Some(DispatchMode::Lua)
    } else if r.contains("invalid dispatcher") {
        Some(DispatchMode::Legacy)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zenvy_proto::Address;

    fn a() -> Address {
        Address::parse("55f131746e40").unwrap()
    }

    #[test]
    fn lua_strings_match_what_was_verified_live() {
        assert_eq!(
            lua(&DesktopCommand::SwitchWorkspace { id: 3 }),
            "hl.dsp.focus({ workspace = 3 })"
        );
        assert_eq!(
            lua(&DesktopCommand::FocusWindow { address: a() }),
            r#"hl.dsp.focus({ window = "address:0x55f131746e40" })"#
        );
        assert_eq!(
            lua(&DesktopCommand::CloseWindow { address: a() }),
            r#"hl.dsp.window.close({ window = "address:0x55f131746e40" })"#
        );
        assert_eq!(
            lua(&DesktopCommand::ToggleFloating { address: a() }),
            r#"hl.dsp.window.float({ action = "toggle", window = "address:0x55f131746e40" })"#
        );
    }

    #[test]
    fn move_does_not_follow_the_window() {
        let s = lua(&DesktopCommand::MoveWindowToWorkspace {
            address: a(),
            workspace: 9,
        });
        // The workspace MUST be a quoted string: with an integer, Hyprland 0.56.2 ignores `follow = false`
        // and switches the view to the target workspace (verified live, PLAN.md Appendix A).
        assert_eq!(
            s,
            r#"hl.dsp.window.move({ workspace = "9", follow = false, window = "address:0x55f131746e40" })"#
        );
    }

    #[test]
    fn legacy_strings() {
        assert_eq!(
            legacy(&DesktopCommand::SwitchWorkspace { id: 3 }),
            "workspace 3"
        );
        assert_eq!(
            legacy(&DesktopCommand::CloseWindow { address: a() }),
            "closewindow address:0x55f131746e40"
        );
    }

    #[test]
    fn classify_real_and_legacy_replies() {
        let lua_reply = "error: [string \"return hl.dispatch(zenvy)\"]:1: ')' expected near 'x'\n\n → Note: dispatch in lua is a shorthand";
        assert_eq!(classify_dispatch_reply(lua_reply), Some(DispatchMode::Lua));
        assert_eq!(
            classify_dispatch_reply("Invalid dispatcher"),
            Some(DispatchMode::Legacy)
        );
        assert_eq!(classify_dispatch_reply("ok"), None);
    }
}
