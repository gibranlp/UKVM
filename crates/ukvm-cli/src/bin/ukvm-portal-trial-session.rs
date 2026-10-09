//! Development launcher; shares production session validation.
#[path = "common/portal_session.rs"]
mod portal_session;

fn main() -> std::process::ExitCode {
    portal_session::run()
}
