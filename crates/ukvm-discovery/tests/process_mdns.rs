//! Opt-in live multicast tests. The browser and advertiser use separate OS
//! processes, unlike the unit smoke test. CI without multicast skips these.
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    time::Duration,
};
use ukvm_discovery::{Advertisement, DiscoveryService};

fn advertisement() -> Advertisement {
    Advertisement {
        node_name: format!("ukvm-process-test-{}", std::process::id()),
        node_id: format!("{:064x}", std::process::id()),
        pairing_port: 24801,
        control_port: 24800,
        pairing_version: 2,
        input_version: (1, 1),
    }
}

#[test]
#[ignore = "requires live local multicast"]
fn advertiser_child() {
    if std::env::var_os("UKVM_MDNS_TEST_CHILD").is_none() {
        return;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let service = DiscoveryService::new(None).unwrap();
        let _guard = service.advertise(&advertisement()).await.unwrap();
        println!("UKVM_MDNS_READY");
        std::io::stdout().flush().unwrap();
        tokio::time::sleep(Duration::from_secs(12)).await;
    });
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "requires live local multicast"]
async fn separate_process_advertisement_and_browse() {
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "advertiser_child", "--ignored", "--nocapture"])
        .env("UKVM_MDNS_TEST_CHILD", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(child);
    let mut reader = BufReader::new(child.0.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "advertiser exited before registration"
        );
        if line.contains("UKVM_MDNS_READY") {
            break;
        }
    }
    let browser = DiscoveryService::new(None).unwrap();
    let results = browser.browse_for(Duration::from_secs(4)).await.unwrap();
    assert!(results.iter().any(|entry| {
        entry.display_name.starts_with("ukvm-process-test-")
            && entry.pairing_version == 2
            && entry.addresses.iter().any(|addr| addr.port() == 24801)
    }));
}

#[tokio::test]
#[ignore = "requires live local multicast sockets"]
async fn registration_without_eligible_interface_fails_visibly() {
    let service = DiscoveryService::new(Some("ukvm-no-such-interface-93847")).unwrap();
    let error = match service.advertise(&advertisement()).await {
        Ok(_) => panic!("registration unexpectedly announced on a missing interface"),
        Err(error) => error,
    };
    assert!(error.contains("did not announce"), "{error}");
}
