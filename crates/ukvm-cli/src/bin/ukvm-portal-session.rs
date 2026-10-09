//! Session-bound launcher for managed Hyprland portal packages.
#[path = "common/portal_session.rs"]
mod portal_session;

fn main() -> std::process::ExitCode {
    portal_session::run()
}
