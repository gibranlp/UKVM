//! Development helper, copied into the reversible portal trial's private user data directory.
fn main() -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        let args: Vec<_> = std::env::args().skip(1).collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            use ukvm_platform::linux_wayland::portal_trial;
            match args.as_slice() {
                [action, binary] if action == "backend" || action == "frontend" => portal_trial::launch(binary, action == "backend").await,
                [action] if action == "check-backend" => portal_trial::check(true).await,
                [action] if action == "check-frontend" => portal_trial::check(false).await,
                [action] if action == "prepare-environment" => portal_trial::prepare_environment().await,
                _ => Err("usage: ukvm-portal-trial-session backend|frontend EXECUTABLE | check-backend | check-frontend | prepare-environment".into()),
            }
        });
        match result {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Trial portal not ready: {error}");
                std::process::ExitCode::from(75)
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("The development portal trial requires Linux.");
        std::process::ExitCode::FAILURE
    }
}
