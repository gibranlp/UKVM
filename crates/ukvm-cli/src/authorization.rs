use serde_json::{json, Value};

pub fn status(remote_version: Option<u32>, capture_version: Option<u32>) -> Value {
    #[cfg(target_os = "linux")]
    {
        use ukvm_platform::linux_wayland::authorization::{self, Kind};
        let view = |kind, version| match authorization::status(kind, version) {
            Ok(value) => serde_json::to_value(value).expect("safe authorization status"),
            Err(error) => {
                json!({"state": "unavailable", "active": false, "remembered": false, "persistence_api_supported": null, "reason": "storage_unavailable", "error": error})
            }
        };
        json!({"remote_desktop": view(Kind::RemoteDesktop, remote_version), "input_capture": view(Kind::InputCapture, capture_version)})
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (remote_version, capture_version);
        json!({"remote_desktop": {"state": "unsupported"}, "input_capture": {"state": "unsupported"}})
    }
}

pub fn print_status(data: &Value) {
    for (key, label) in [
        ("remote_desktop", "Remote control authorization"),
        ("input_capture", "Input capture authorization"),
    ] {
        let value = &data[key];
        let state = match value["state"].as_str().unwrap_or("unavailable") {
            "persistent" => "Persistent (remembered; restoration remains portal-controlled)",
            "active" => "Active (this session only)",
            "needs_approval" => "Needs approval",
            "unsupported" => "Unsupported (persistent authorization unavailable)",
            _ => "Unavailable (inspect private state permissions)",
        };
        println!("{label}: {state}");
        if let Some(error) = value["error"].as_str() {
            println!("  Problem: {error}");
        }
    }
}
