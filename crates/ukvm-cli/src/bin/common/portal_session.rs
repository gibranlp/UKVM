//! Session validation for managed Hyprland portal services.
pub fn run() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        let args: Vec<_> = std::env::args().skip(1).collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            use ukvm_platform::linux_wayland::portal_session;
            match args.as_slice() {
                [action, binary] if action == "backend" || action == "frontend" => portal_session::launch(binary, action == "backend").await,
                [action] if action == "check-backend" => portal_session::check(true).await,
                [action] if action == "check-frontend" => portal_session::check(false).await,
                [action] if action == "prepare-environment" => portal_session::prepare_environment().await,
                _ => Err("usage: ukvm-portal-session backend|frontend EXECUTABLE | check-backend | check-frontend | prepare-environment".into()),
            }
        });
        match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Portal not ready: {error}");
                std::process::ExitCode::from(75)
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("Managed Hyprland portals require Linux.");
        std::process::ExitCode::FAILURE
    }
}
