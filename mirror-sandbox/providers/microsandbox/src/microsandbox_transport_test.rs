use std::path::PathBuf;

use assert_matches::assert_matches;
use pretty_assertions::assert_eq;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::{sandboxd_file, EgressHost, FakeMsb, MicrosandboxConfig};

const NAME: &str = "mirror-ws-test";

/// An `msb inspect` document whose status the fake fills in.
fn document(default_egress: &str) -> Value {
    json!({
        "status": "__STATUS__",
        "config": {
            "labels": {},
            "network": {
                "policy": {
                    "default_egress": default_egress,
                    "default_ingress": "deny",
                    "rules": [],
                },
                "ports": [],
            },
            "mounts": [
                {"host": "/workspace", "guest": "/workspace", "options": {"readonly": false}},
                {
                    "host": sandboxd_file(),
                    "guest": "/usr/local/bin/mirror-sandboxd",
                    "options": {"readonly": true},
                },
            ],
        },
        "active_config": null,
    })
}

async fn provider(fake: &FakeMsb) -> MicrosandboxProvider {
    let config = MicrosandboxConfig {
        image: "debian:bookworm-slim".to_string(),
        workspace: PathBuf::from("/workspace"),
        sandboxd: PathBuf::from("/opt/mirror/mirror-sandboxd"),
        egress: Vec::<EgressHost>::new(),
        cpus: None,
        memory_mib: None,
    };
    MicrosandboxProvider::new(fake.msb().await, config).expect("valid config")
}

#[tokio::test]
async fn connect_starts_sandboxd_on_the_workspace_with_the_file_cache_refresh() {
    let fake = FakeMsb::new();
    fake.add_sandbox(NAME, "Running", &document("deny"));
    let provider = provider(&fake).await;

    let mut stream = provider.connect(NAME, "/workspace").await.expect("connect");
    stream.writer.write_all(b"ping\n").await.expect("write");
    let mut echoed = [0; 5];
    stream.reader.read_exact(&mut echoed).await.expect("read");

    assert_eq!(&echoed, b"ping\n");
    assert_eq!(
        fake.calls().last().map(String::as_str),
        Some(
            "exec --quiet --stream --env MIRROR_WORKSPACE=/workspace \
             --env MIRROR_REFRESH_FILE_CACHE=1 mirror-ws-test -- \
             /usr/local/bin/mirror-sandboxd serve"
        )
    );
}

#[tokio::test]
async fn connect_refuses_a_stopped_vm_rather_than_booting_a_throwaway_one() {
    let fake = FakeMsb::new();
    fake.add_sandbox(NAME, "Stopped", &document("deny"));
    let provider = provider(&fake).await;

    assert_matches!(
        provider.connect(NAME, "/workspace").await,
        Err(SandboxTransportError::Suspended(_))
    );
    assert!(!fake.calls().iter().any(|call| call.starts_with("exec ")));
}

#[tokio::test]
async fn connect_refuses_a_vm_whose_network_policy_changed() {
    let fake = FakeMsb::new();
    fake.add_sandbox(NAME, "Running", &document("allow"));
    let provider = provider(&fake).await;

    assert_matches!(
        provider.connect(NAME, "/workspace").await,
        Err(SandboxTransportError::Failed(detail)) if detail.contains("does not deny network traffic")
    );
}

#[tokio::test]
async fn connect_refuses_a_workspace_the_vm_does_not_mount() {
    let fake = FakeMsb::new();
    fake.add_sandbox(NAME, "Running", &document("deny"));
    let provider = provider(&fake).await;

    assert_matches!(
        provider.connect(NAME, "/").await,
        Err(SandboxTransportError::Failed(detail)) if detail.contains("not the workspace \"/\"")
    );
    assert!(!fake.calls().iter().any(|call| call.starts_with("exec ")));
}

#[tokio::test]
async fn connect_to_a_missing_vm_reports_it_gone() {
    let fake = FakeMsb::new();
    let provider = provider(&fake).await;

    assert_matches!(
        provider.connect(NAME, "/workspace").await,
        Err(SandboxTransportError::Gone(_))
    );
}
