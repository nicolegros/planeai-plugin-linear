use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// PlaneAI keeps stdin open after `plugin.shutdown` and kills a sidecar that has not exited within three seconds.
#[test]
fn the_sidecar_exits_after_shutdown_while_stdin_stays_open() {
    let state = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_planeai-plugin-linear"))
        .env("PLANEAI_PLUGIN_DATA_DIR", state.path().join("data"))
        .env("PLANEAI_PLUGIN_SECRETS_DIR", state.path().join("secrets"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"plugin.handshake","params":{{"host_api_version":"planeai.plugin-host.v7"}}}}"#).unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"plugin.shutdown"}}"#
    )
    .unwrap();
    let mut lines = Vec::new();
    for _ in 0..2 {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        lines.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
    }
    assert_eq!(lines[0]["result"]["plugin_id"], "linear");
    assert_eq!(
        lines[1],
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "result": { "stopping": true } })
    );

    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    if status.is_none() {
        child.kill().unwrap();
    }
    assert!(
        status.is_some_and(|status| status.success()),
        "sidecar still running after shutdown"
    );
}
