//! Native-core protocol fixtures stand in for the separately installed core.

#[tokio::test]
async fn native_core_refreshes_topology_on_subscription_events_and_missed_snapshots() {
    use tokio_stream::StreamExt;
    let core = CoreFixture::start();
    let backend = HerdrCoreBackend::with_command(core.socket.path.to_str().unwrap(), &[]);
    let mut stream = backend.activity_stream().await.unwrap();
    for _ in 0..3 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(event.name, "layout_updated");
        assert_eq!(event.payload["event"], "layout_updated");
    }
    let requests = core.requests.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r["method"] == "events.subscribe")
            .count(),
        1
    );
    assert!(!requests
        .iter()
        .any(|r| r["method"] == "doc.set" || r["method"] == "terminal.run"));
}

#[tokio::test]
async fn native_core_advertises_its_optional_backend_without_legacy_collaboration() {
    let kind: BackendKind = serde_json::from_value(json!("herdr-core")).unwrap();
    let capabilities = crate::session_capabilities(kind, true, Some("0.1.18"));
    assert!(capabilities.contains(&"native_herdr_core_v1"));
    assert!(!capabilities.contains(&"agent_collaboration"));
    assert!(crate::session_capabilities(kind, false, Some("0.1.18")).is_empty());
}

#[tokio::test]
async fn native_core_delivers_named_keys_through_the_core_encoder() {
    let core = CoreFixture::start();
    let backend = HerdrCoreBackend::with_command(core.socket.path.to_str().unwrap(), &[]);
    let pane = PaneId::new("hcp_d29ya2VyL3Q3");
    backend
        .send_keys(
            &pane,
            &[
                "Enter".into(),
                "ctrl+C".into(),
                "ArrowUp".into(),
                "X".into(),
            ],
        )
        .await
        .unwrap();
    let requests = core.requests.lock().unwrap();
    let keys: Vec<_> = requests
        .iter()
        .filter(|r| r["method"] == "terminal.keys")
        .collect();
    assert_eq!(keys.len(), 1);
    assert_eq!(
        keys[0]["params"],
        json!({"terminal":"worker/t7","keys":["enter","ctrl+c","up","shift+x"]})
    );
}

#[tokio::test]
async fn native_core_rejects_aliases_and_malformed_wire_ids_before_connecting() {
    use base64::Engine as _;
    let backend = HerdrCoreBackend::with_command("/no-native-core.sock", &[]);
    for address in [
        "worker/alias",
        "worker/../t7",
        "worker/t7;rm",
        "worker/T7",
        "",
    ] {
        let pane = PaneId::new(format!(
            "hcp_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(address)
        ));
        let result = backend.send_text(&pane, "x", SendTextMode::Keys).await;
        assert!(
            matches!(result, Err(BackendError::InvalidTarget(_))),
            "{address}"
        );
    }
}

#[tokio::test]
async fn native_core_pastes_without_submitting_and_types_keys_without_paste_markers() {
    use base64::Engine as _;
    for (mode, bracketed) in [
        (SendTextMode::Paste, false),
        (SendTextMode::Paste, true),
        (SendTextMode::Keys, true),
    ] {
        let socket = TestSocket::new();
        let listener = tokio::net::UnixListener::bind(&socket.path).unwrap();
        let serving = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let hello: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(hello["method"], "hello");
            assert_eq!(hello["params"]["framed"], true);
            let reply = json!({"jsonrpc":"2.0","id":hello["id"],"result":{
                "generation":1,"framed":true,"version":"0.1.18","core":{"id":"fixture","name":"host"},"features":[],"methods":[]
            }});
            stream
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
            loop {
                let (channel, kind, payload) = read_test_frame(&mut stream).await;
                assert_eq!((channel, kind), (0, 0));
                let request: Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(request["params"]["terminal"], "worker/t7");
                let result = match request["method"].as_str().unwrap() {
                    "terminal.get" => json!({"cols":120,"rows":40}),
                    "terminal.attach" => {
                        assert_eq!(request["params"]["cols"], 120);
                        assert_eq!(request["params"]["rows"], 40);
                        assert_eq!(request["params"]["resumed"], true);
                        assert_eq!(request["params"]["history"], 0);
                        json!({"channel":1,"terminal":{"cols":120,"rows":40}})
                    }
                    method => panic!("unexpected method: {method}"),
                };
                let reply = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
                write_test_frame(&mut stream, 0, 0, reply.to_string().as_bytes()).await;
                if request["method"] == "terminal.attach" {
                    break;
                }
            }
            let paint = if bracketed {
                b"\x1b[?2004h\x1b[?9001l"
            } else {
                b"\x1b[?2004l\x1b[?9001l"
            };
            write_test_frame(&mut stream, 1, 0, paint).await;
            let mut input = Vec::new();
            loop {
                let (channel, kind, payload) = read_test_frame(&mut stream).await;
                assert_eq!(channel, 1);
                match kind {
                    0 => input.extend(payload),
                    1 => {
                        assert_eq!(payload.len(), 4);
                    }
                    2 => {
                        write_test_frame(&mut stream, 1, 2, &[]).await;
                        break;
                    }
                    _ => panic!("input must not send resize, focus or automatic-input frames"),
                }
            }
            input
        });
        let backend = HerdrCoreBackend::with_command(socket.path.to_str().unwrap(), &[]);
        let pane = PaneId::new(format!(
            "hcp_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("worker/t7")
        ));
        backend.send_text(&pane, "alpha\nbeta", mode).await.unwrap();
        let input = serving.await.unwrap();
        assert_eq!(
            input,
            if bracketed && mode == SendTextMode::Paste {
                b"\x1b[200~alpha\nbeta\x1b[201~".as_slice()
            } else {
                b"alpha\nbeta".as_slice()
            }
        );
    }
}

async fn read_test_frame<S: tokio::io::AsyncRead + Unpin>(stream: &mut S) -> (u32, u8, Vec<u8>) {
    use tokio::io::AsyncReadExt;
    let mut header = [0; 9];
    stream.read_exact(&mut header).await.unwrap();
    let len = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;
    assert!(len <= 16 * 1024 * 1024);
    let mut payload = vec![0; len];
    stream.read_exact(&mut payload).await.unwrap();
    (
        u32::from_le_bytes(header[..4].try_into().unwrap()),
        header[4],
        payload,
    )
}

async fn write_test_frame<S: tokio::io::AsyncWrite + Unpin>(
    stream: &mut S,
    channel: u32,
    kind: u8,
    payload: &[u8],
) {
    let mut frame = channel.to_le_bytes().to_vec();
    frame.push(kind);
    frame.extend((payload.len() as u32).to_le_bytes());
    frame.extend(payload);
    stream.write_all(&frame).await.unwrap();
    stream.flush().await.unwrap();
}

#[tokio::test]
async fn native_core_can_use_an_explicit_stdio_bridge_without_a_shell_interpretation() {
    let hello = serde_json::json!({"jsonrpc":"2.0","id":1,"result":{
        "generation":1,"version":"0.1.18","core":{"id":"bridge","name":"host"},
        "framed":false,"methods":[],"features":[]
    }});
    let session: crate::SessionConfig = serde_json::from_value(serde_json::json!({
        "id":"bridge", "label":"Bridge", "backend":"herdr-core", "socket_path":"/no-such-native-core.sock",
        "core_command":["/bin/sh", "-c", "read -r request; printf '%s\\n' \"$1\"; cat >/dev/null", "fixture", hello.to_string()]
    })).unwrap();
    let metadata = crate::terminal_backend(&session).metadata().await.unwrap();
    assert_eq!(metadata.kind.as_str(), "herdr-core");
    assert_eq!(metadata.protocol, Some(1));
}

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::*;
use crate::test_support::TestSocket;

struct CoreFixture {
    socket: TestSocket,
    requests: Arc<Mutex<Vec<Value>>>,
    serving: tokio::task::JoinHandle<()>,
}

impl CoreFixture {
    fn start() -> Self {
        let socket = TestSocket::new();
        let listener = tokio::net::UnixListener::bind(&socket.path).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let serving = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let (reader, mut writer) = stream.into_split();
                    let mut lines = BufReader::new(reader).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let request: Value = serde_json::from_str(&line).unwrap();
                        assert_eq!(request["jsonrpc"], "2.0");
                        recorded.lock().unwrap().push(request.clone());
                        let terminal = json!({
                            "address": "worker/t7", "labels": {"herdr.space":"sp1", "herdr.tab":"tb1"},
                            "argv": ["pi"], "cwd": "/work/project", "state": "live", "created_ms": 123,
                            "screen": "exact", "cols": 120, "rows": 40, "pid": 42,
                            "status": {"app":"pi", "state":"idle"}, "title":"Agent"
                        });
                        let result = match request["method"].as_str().unwrap() {
                            "hello" => {
                                json!({"generation":1,"version":"0.1.18","core":{"id":"fixture","name":"host"},"framed":false,"methods":["terminal.list","terminal.get","terminal.read","doc.get"],"features":[]})
                            }
                            "terminal.list" => {
                                json!({"terminals":[terminal],"hidden_finished":0,"machines":[]})
                            }
                            "terminal.get" => terminal,
                            "terminal.keys" => json!({"turn":1}),
                            "events.subscribe" => json!({"seq":1,"terminals":[]}),
                            "doc.get" => {
                                json!({"version":1,"value":{"spaces":[{"id":"sp1","name":"Project","folder":"/work/project","tabs":[{"id":"tb1","name":"Review"}]}]}})
                            }
                            "terminal.read" => {
                                assert_eq!(request["params"]["terminal"], "worker/t7");
                                assert_eq!(request["params"]["lines"], 0);
                                json!({"text":"native output", "complete":true,"how":"screen"})
                            }
                            method => panic!("unexpected core method: {method}"),
                        };
                        let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
                        if writer
                            .write_all(format!("{response}\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                        if request["method"] == "events.subscribe" {
                            for notification in [
                                json!({"jsonrpc":"2.0","method":"event","params":{"seq":2,"type":"created","terminal":"worker/t7","data":{}}}),
                                json!({"jsonrpc":"2.0","method":"missed","params":{"seq":5,"terminals":[]}}),
                            ] {
                                if writer
                                    .write_all(format!("{notification}\n").as_bytes())
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        Self {
            socket,
            requests,
            serving,
        }
    }
}

impl Drop for CoreFixture {
    fn drop(&mut self) {
        self.serving.abort();
    }
}

#[tokio::test]
async fn native_core_browses_existing_remote_projects_without_changing_the_desktop() {
    let core = CoreFixture::start();
    let session: crate::SessionConfig = serde_json::from_value(json!({
        "id":"native", "label":"Native", "backend":"herdr-core", "socket_path":core.socket.path
    }))
    .unwrap();
    let backend = crate::terminal_backend(&session);
    let (metadata, compatibility) =
        crate::platform::metadata::backend_metadata(&session, &*backend).await;
    assert_eq!(metadata["kind"], "herdr-core");
    assert_eq!(metadata["protocol"], 1);
    assert_eq!(compatibility["connected"], true);
    assert_eq!(compatibility["compatible"], true);
    let workspaces = backend.list_workspaces().await.unwrap();
    assert_eq!(workspaces.len(), 1);
    assert!(workspaces[0].label.contains("Project"));
    let tabs = backend.list_tabs().await.unwrap();
    assert_eq!(tabs[0].label, "Review");
    let panes = backend.list_panes().await.unwrap();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].workspace_id, workspaces[0].id);
    assert_eq!(panes[0].tab_id, tabs[0].id);
    assert_eq!(panes[0].width, Some(120));
    // A core's filesystem namespace is not proof of a path on this Gateway.
    assert!(panes[0].cwd.is_none());
    assert!(workspaces[0].checkout_path.is_none());
    assert_eq!(panes[0].agent.as_deref(), Some("pi"));
    assert!(panes[0]
        .id
        .as_str()
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    let output = backend
        .read_pane(&ReadPane {
            pane_id: panes[0].id.clone(),
            source: OutputSource::Recent,
            format: OutputFormat::Text,
            lines: 500,
            start: None,
            end: None,
        })
        .await
        .unwrap();
    assert_eq!(output.text, "native output");
    assert!(output.range.is_none());
    let requests = core.requests.lock().unwrap();
    assert!(requests.iter().all(|r| matches!(
        r["method"].as_str(),
        Some("hello" | "terminal.list" | "terminal.get" | "terminal.read" | "doc.get")
    )));
}
