use std::process::ExitCode;

mod discovery;
mod network;
mod pairing;
mod settings;

const HELP: &str = "UniversalKVM\n\
Usage: ukvm <command>\n\
Commands:\n\
  version   Show executable and protocol versions\n\
  config [path|check|show]  Inspect configuration\n\
  peer list|show|add|remove|permissions|allow|deny  Manage trusted peers\n\
  layout [set EDGE PEER|remove EDGE]  Inspect or edit layout\n\
  control EDGE --debug [--zone INDEX]  Forward captured input to a configured DebugSink\n\
  doctor    Probe the current graphical session and portal capabilities\n\
  platform wayland zones               Request capture permission and list zones\n\
  platform wayland capture <edge> [zone] [--verbose]  Test local capture\n\
  platform wayland capture <edge> [zone] --send HOST --peer-cert PATH  Send to DebugSink\n\
  identity [--state-dir DIR]  Show or create persistent node identity\n\
  listen [--peer-cert PATH] [--bind ADDR] [--state-dir DIR]  Authenticated DebugSink\n\
  connect PEER [--peer-cert PATH] [--state-dir DIR]  Check peer connection\n\
  debug send-test-events HOST --peer-cert PATH  Send safe synthetic debug events\n\
  status    Show node, peers, layout, and capabilities\n\
  start     Start UKVM (not implemented yet)\n\
  stop      Stop UKVM (not implemented yet)\n\
  pair [HOST[:PORT]]  Temporarily pair (no input permission)\n\
  pair --discover [--interface NAME]  Find and select a pairing node\n\
  pair --interface NAME  Advertise pairing on one interface\n\
  pair --bind ADDR:PORT  Test with an explicit temporary pairing listener\n\
  pair status  Inspect pairing recovery records\n\
  pair recover [HOST[:PORT] [TRANSACTION_ID]]  Reconcile confirmed pairing\n\
  pair cleanup  Remove expired recovery records\n\
  discover [--interface NAME]  Browse nearby pairing nodes for four seconds\n\
  devices   List peers (not implemented yet)\n\
  help      Show this help";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        [command] if command == "help" || command == "--help" || command == "-h" => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        [command] if command == "version" || command == "--version" || command == "-V" => {
            println!(
                "ukvm {} (protocol {}.{})",
                env!("CARGO_PKG_VERSION"),
                ukvm_protocol::PROTOCOL_MAJOR,
                ukvm_protocol::PROTOCOL_MINOR
            );
            ExitCode::SUCCESS
        }
        [command, rest @ ..] if command == "config" => settings::config(rest),
        [command, rest @ ..] if command == "peer" => settings::peer(rest),
        [command, rest @ ..] if command == "layout" => settings::layout(rest),
        [command, rest @ ..] if command == "pair" => pairing::pair(rest),
        [command, rest @ ..] if command == "discover" => discovery::discover(rest),
        [command, edge, rest @ ..] if command == "control" => network::control(edge, rest),
        [command] if command == "status" => settings::status(),
        [command] if command == "doctor" => doctor(),
        [command, rest @ ..] if command == "identity" => network::identity(rest),
        [command, rest @ ..] if command == "listen" => network::listen(rest),
        [command, target, rest @ ..] if command == "connect" => {
            network::connect(target, rest, false)
        }
        [debug, send, target, rest @ ..] if debug == "debug" && send == "send-test-events" => {
            network::connect(target, rest, true)
        }
        #[cfg(target_os = "linux")]
        [platform, wayland, zones]
            if platform == "platform" && wayland == "wayland" && zones == "zones" =>
        {
            if !ukvm_platform::linux_wayland::SessionInfo::detect().wayland_socket_reachable {
                return fail("no reachable Wayland session socket");
            }
            with_runtime(async {
                eprintln!("Requesting InputCapture session; your desktop may ask for permission.");
                match ukvm_platform::linux_wayland::zones().await {
                    Ok(set) => {
                        println!("Wayland InputCapture Zones\n==========================");
                        for (index, zone) in set.zones.iter().enumerate() {
                            println!(
                                "\nZone {index}\n  position: {}, {}\n  size: {}x{}",
                                zone.x, zone.y, zone.width, zone.height
                            );
                            println!(
                                "  coordinates: left={} right={} top={} bottom={}",
                                zone.x,
                                i64::from(zone.x) + i64::from(zone.width),
                                zone.y,
                                i64::from(zone.y) + i64::from(zone.height)
                            );
                        }
                        println!("Zone set: {}", set.id);
                        match ukvm_platform::linux_wayland::all_outer_segments(&set.zones) {
                            Ok(segments) => {
                                println!("\nAvailable outer segments:");
                                for segment in segments {
                                    println!(
                                        "  Zone {} {}: ({}, {}) to ({}, {})",
                                        segment.zone,
                                        segment.edge.as_str(),
                                        segment.start.0,
                                        segment.start.1,
                                        segment.end.0,
                                        segment.end.1
                                    );
                                }
                                ExitCode::SUCCESS
                            }
                            Err(err) => fail(&err),
                        }
                    }
                    Err(err) => fail(&err),
                }
            })
        }
        #[cfg(target_os = "linux")]
        [platform, wayland, capture, edge, rest @ ..]
            if platform == "platform" && wayland == "wayland" && capture == "capture" =>
        {
            if !ukvm_platform::linux_wayland::SessionInfo::detect().wayland_socket_reachable {
                return fail("no reachable Wayland session socket");
            }
            use ukvm_platform::linux_wayland::Edge;
            let edge = match edge.as_str() {
                "left" => Edge::Left,
                "right" => Edge::Right,
                "top" => Edge::Top,
                "bottom" => Edge::Bottom,
                _ => return fail("edge must be left, right, top, or bottom"),
            };
            let mut zone = None;
            let mut verbose = false;
            let mut target = None;
            let mut net_args = Vec::new();
            let mut iter = rest.iter();
            while let Some(value) = iter.next() {
                match value.as_str() {
                    "--verbose" if !verbose => verbose = true,
                    "--send" if target.is_none() => {
                        target = match iter.next() {
                            Some(value) => Some(value.clone()),
                            None => return fail("--send requires a peer address"),
                        };
                    },
                    "--peer-cert" | "--state-dir" | "--name" => {
                        let Some(argument) = iter.next() else { return fail(&format!("missing value for {value}")); };
                        net_args.push(value.clone());
                        net_args.push(argument.clone());
                    }
                    _ if zone.is_none() => zone = match value.parse::<usize>() {
                        Ok(index) => Some(index),
                        Err(_) => return fail("usage: ukvm platform wayland capture <edge> [zone] [--send HOST --peer-cert PATH] [--verbose]"),
                    },
                    _ => return fail("invalid capture arguments"),
                }
            }
            if let Some(target) = target {
                return network::capture_send(edge, zone, &target, &net_args, verbose);
            }
            if !net_args.is_empty() {
                return fail("network options require --send HOST");
            }
            with_runtime(async move {
                println!("UniversalKVM Wayland Capture Test\n=================================");
                println!(
                    "Requesting InputCapture permission. No keyboard contents will be printed."
                );
                let mut report = |notice| print_capture_notice(notice, verbose);
                match ukvm_platform::linux_wayland::capture(
                    edge,
                    zone,
                    std::time::Duration::from_secs(60),
                    &mut report,
                )
                .await
                {
                    Ok(summary) => {
                        println!(
                            "\nCapture summary\n  Activated: {}  Deactivated: {}  EI starts: {}",
                            summary.activations, summary.deactivations, summary.eis_starts
                        );
                        println!(
                            "  Pointer: {}  Buttons: {}  Scroll: {}  Keys: {}",
                            summary.pointer_events,
                            summary.button_events,
                            summary.scroll_events,
                            summary.key_events
                        );
                        println!(
                            "  Release: {}",
                            match summary.release_succeeded {
                                Some(true) => "succeeded",
                                Some(false) => "failed",
                                None => "not needed (never active)",
                            }
                        );
                        if summary.timed_out {
                            println!("  Safety timeout reached.");
                        }
                        ExitCode::SUCCESS
                    }
                    Err(err) => fail(&err),
                }
            })
        }
        [command, ..] if matches!(command.as_str(), "start" | "stop" | "devices") => {
            fail(&format!("{command} is not implemented yet"))
        }
        _ => {
            eprintln!("Unknown command or arguments.\n{HELP}");
            ExitCode::FAILURE
        }
    }
}

fn fail(message: &str) -> ExitCode {
    eprintln!("ukvm: {message}");
    ExitCode::FAILURE
}

fn doctor() -> ExitCode {
    println!("UniversalKVM Doctor\n===================");
    println!("System\n  OS: {}", std::env::consts::OS);
    #[cfg(target_os = "linux")]
    {
        let report = with_runtime(ukvm_platform::linux_wayland::probe());
        let session = &report.session;
        println!(
            "  Session hint: {}",
            session.session_type_hint.as_deref().unwrap_or("unknown")
        );
        println!(
            "  Desktop hint: {}",
            session.desktop_hint.as_deref().unwrap_or("unknown")
        );
        println!(
            "  Wayland display: {}",
            session.wayland_display.as_deref().unwrap_or("none")
        );
        println!(
            "  Wayland socket reachable: {}",
            yes_no(session.wayland_socket_reachable)
        );
        println!(
            "  X11 display hint: {}",
            session.x11_display_hint.as_deref().unwrap_or("none")
        );
        println!(
            "\nXDG Desktop Portal\n  Service: {}",
            yes_no(report.portal_service)
        );
        if let Some(error) = &report.portal_error {
            println!("  Problem: {error}");
        }
        show_interface("InputCapture", &report.capture);
        show_interface("RemoteDesktop", &report.remote);
        let injection =
            ukvm_platform::linux_wayland::InjectionPortalCapability::from_probe(&report.remote);
        let controller_advertised = session.wayland_socket_reachable
            && report.capture.version.is_some()
            && report.capture.pointer_advertised;
        println!("\nUniversalKVM Capabilities");
        println!("  Controller");
        println!(
            "    InputCapture portal: {}",
            yes_no(report.capture.version.is_some())
        );
        println!(
            "    Pointer advertised: {}",
            yes_no(report.capture.pointer_advertised)
        );
        println!(
            "    Keyboard advertised: {}",
            yes_no(report.capture.keyboard_advertised)
        );
        println!("    EIS receive: implemented; live connection not tested by doctor");
        println!("    Physical capture proof: not measured by this read-only command");
        println!("  Controlled node");
        println!("    DebugSink: implemented; authenticated LAN delivery physically proven");
        println!(
            "    RemoteDesktop portal: {}",
            yes_no(injection.portal_available)
        );
        println!(
            "    ConnectToEIS API: {}",
            yes_no(injection.eis_api_available)
        );
        println!(
            "    Pointer advertised: {}",
            yes_no(injection.pointer_advertised)
        );
        println!(
            "    Keyboard advertised: {}",
            yes_no(injection.keyboard_advertised)
        );
        println!(
            "    EIS sender: {}",
            if injection.eis_api_available {
                "API advertised; UKVM sender not implemented or tested"
            } else {
                "unavailable through this portal"
            }
        );
        let evidence = ukvm_platform::linux_wayland::injection_evidence(&report.remote);
        for (name, status) in [
            ("Relative pointer", evidence.pointer),
            ("Button", evidence.button),
            ("Scroll", evidence.scroll),
            ("Keyboard", evidence.keyboard),
        ] {
            println!(
                "    {name} injection: detected={} implemented={} authorized={} physically_proven={}",
                yes_no(status.detected), yes_no(status.implemented),
                yes_no(status.authorized), yes_no(status.physically_proven)
            );
        }
        println!(
            "    Pointer injection: {}",
            if injection.pointer_advertised {
                "advertised; unproven"
            } else {
                "unavailable"
            }
        );
        println!(
            "    Keyboard injection: {}",
            if injection.keyboard_advertised {
                "advertised; unproven"
            } else {
                "unavailable"
            }
        );
        println!("\nStatus");
        println!(
            "  Controller: {}",
            if controller_advertised {
                "AVAILABLE; use the opt-in capture test for physical proof"
            } else {
                "UNAVAILABLE"
            }
        );
        println!(
            "  Controlled node: {}",
            if !injection.portal_available {
                "UNAVAILABLE (RemoteDesktop portal absent)"
            } else if !injection.pointer_advertised && !injection.keyboard_advertised {
                "UNAVAILABLE (no input devices advertised)"
            } else {
                "UNPROVEN (portal advertised; UKVM injection not implemented)"
            }
        );
        if !session.wayland_socket_reachable {
            println!("\nRecommendation: start UKVM inside a reachable Wayland user session.");
        } else if !report.portal_service {
            println!("\nRecommendation: check access to the user session bus and desktop portal service.");
        } else if !report.capture.pointer_advertised {
            println!(
                "\nRecommendation: enable a desktop portal backend that implements InputCapture."
            );
        } else {
            println!("\nNext probe: ukvm platform wayland zones");
        }
        if !injection.portal_available {
            println!("Controlled-node blocker: the active XDG Desktop Portal frontend does not expose org.freedesktop.portal.RemoteDesktop.");
        } else if !injection.eis_api_available {
            println!("Controlled-node blocker: this RemoteDesktop version lacks ConnectToEIS (requires version 2).");
        }
    }
    #[cfg(not(target_os = "linux"))]
    println!("Platform diagnostics are not implemented for this OS yet.");
    println!("\nDiscovery\n  Backend: mDNS/DNS-SD (_ukvm._tcp.local.)\n  Publish/browse: implemented\n  Live multicast: not tested by doctor; run 'ukvm discover'\n  Active advertisement: no (only while 'ukvm pair' listens)");
    ExitCode::SUCCESS
}

fn with_runtime<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime")
        .block_on(future)
}

#[cfg(target_os = "linux")]
fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

#[cfg(target_os = "linux")]
fn show_interface(name: &str, probe: &ukvm_platform::linux_wayland::InterfaceProbe) {
    println!("\n{name}");
    match probe.version {
        Some(version) => println!("  Interface version: {version}"),
        None => println!("  Interface: unavailable"),
    }
    println!(
        "  Keyboard advertised: {}",
        yes_no(probe.keyboard_advertised)
    );
    println!("  Pointer advertised: {}", yes_no(probe.pointer_advertised));
    if let Some(error) = &probe.error {
        println!("  Problem: {error}");
    }
}

#[cfg(target_os = "linux")]
fn print_capture_notice(notice: ukvm_platform::linux_wayland::CaptureNotice, verbose: bool) {
    use ukvm_platform::linux_wayland::CaptureNotice;
    match notice {
        CaptureNotice::SessionCreated {
            version,
            keyboard_granted,
        } => {
            println!("Session: created (InputCapture v{version})");
            println!("  Pointer: granted  Keyboard: {}", yes_no(keyboard_granted));
        }
        CaptureNotice::Zones(set) => {
            println!("Zone set: {}", set.id);
            for (index, zone) in set.zones.iter().enumerate() {
                println!(
                    "  Zone {index}: {}x{} @ {},{}",
                    zone.width, zone.height, zone.x, zone.y
                );
            }
        }
        CaptureNotice::EisSocketAcquired => println!("EIS: socket acquired"),
        CaptureNotice::EisContextConnected => {
            if verbose {
                println!("EIS: context connected");
            }
        }
        CaptureNotice::EisHandshakeComplete => println!("EIS: handshake complete"),
        CaptureNotice::SeatAvailable => {
            if verbose {
                println!("EIS: seat announced and capabilities bound");
            }
        }
        CaptureNotice::DeviceAvailable {
            pointer,
            keyboard,
            button,
            scroll,
        } => {
            if verbose {
                println!("EIS: device announced (pointer={pointer}, keyboard={keyboard}, button={button}, scroll={scroll})");
            }
        }
        CaptureNotice::BarriersRequested(items) => {
            println!("Requested barriers:");
            for item in items {
                let s = item.segment;
                println!(
                    "  ID {}: Zone {} {} from ({}, {}) to ({}, {})",
                    item.id,
                    s.zone,
                    s.edge.as_str(),
                    s.start.0,
                    s.start.1,
                    s.end.0,
                    s.end.1
                );
            }
        }
        CaptureNotice::BarrierDecision(decision) => {
            println!("Portal accepted barrier IDs: {:?}", decision.accepted);
            println!("Portal rejected barrier IDs: {:?}", decision.rejected);
        }
        CaptureNotice::Enabled => {
            println!("InputCapture enabled. Move the pointer THROUGH the printed outer edge.");
            println!("Waiting for activation (60-second safety timeout; Esc or Ctrl-C exits). When active, capture releases after 5 seconds.");
        }
        CaptureNotice::Activated {
            activation_id,
            barrier_id,
        } => {
            println!(
                "CAPTURE ACTIVATED: activation_id={activation_id:?}, barrier_id={barrier_id:?}"
            );
            println!("Receiving EI events for up to 5 seconds...");
        }
        CaptureNotice::Deactivated { activation_id } => {
            if verbose {
                println!("[wayland] Deactivated: activation_id={activation_id:?}");
            }
        }
        CaptureNotice::ZonesChanged { zone_set } => {
            println!("[wayland] ZonesChanged: invalidated zone_set={zone_set:?}; stopping capture");
        }
        CaptureNotice::EisStarted { sequence } => {
            if verbose {
                println!("EIS: start emulating, sequence={sequence}");
            }
        }
        CaptureNotice::EisStopped => {
            if verbose {
                println!("EIS: stop emulating");
            }
        }
        CaptureNotice::EventCounts(summary) => {
            println!(
                "Events: pointer={} buttons={} scroll={} keyboard={}",
                summary.pointer_events,
                summary.button_events,
                summary.scroll_events,
                summary.key_events
            );
        }
        CaptureNotice::ReleaseRequested {
            activation_id,
            cursor_position,
        } => {
            println!("Releasing capture: activation_id={activation_id:?}, suggested cursor position={cursor_position:?}");
        }
        CaptureNotice::ReleaseResult(result) => match result {
            Ok(()) => println!("Portal Release: succeeded"),
            Err(err) => println!("Portal Release: failed: {err}"),
        },
        CaptureNotice::Closed(result) => match result {
            Ok(()) => println!("Session: disabled and closed"),
            Err(err) => println!("Session close problem: {err}"),
        },
    }
}
