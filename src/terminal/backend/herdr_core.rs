//! Generation-1 native Herdr core adapter. Layout documents are read-only;
//! selecting a phone view never changes another screen's focus.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::LazyLock;

mod wire;
use wire::Client;

static RPC_SLOTS: LazyLock<tokio::sync::Semaphore> =
    LazyLock::new(|| tokio::sync::Semaphore::new(32));

use super::*;

const LIMIT: u64 = 16 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct HerdrCoreBackend {
    socket: String,
    command: Vec<String>,
}

impl HerdrCoreBackend {
    pub fn with_command(socket: &str, command: &[String]) -> Self {
        Self {
            socket: socket.into(),
            command: command.to_vec(),
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, BackendError> {
        tokio::time::timeout(DEADLINE, async {
            let _slot = RPC_SLOTS
                .acquire()
                .await
                .map_err(|_| BackendError::Unavailable)?;
            let mut client = self.connect(false).await?;
            if method == "hello" {
                return Ok(client.hello.clone());
            }
            client.call(method, params).await
        })
        .await
        .map_err(|_| BackendError::Unavailable)?
    }

    async fn connect(&self, framed: bool) -> Result<Client, BackendError> {
        if let Some(program) = self.command.first() {
            if program.is_empty()
                || self.command.len() > 64
                || self.command.iter().any(|s| s.contains('\0'))
            {
                return Err(BackendError::InvalidResponse("core bridge command"));
            }
            let mut child = tokio::process::Command::new(program)
                .args(&self.command[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|_| BackendError::Unavailable)?;
            let input = child.stdin.take().ok_or(BackendError::Unavailable)?;
            let output = child.stdout.take().ok_or(BackendError::Unavailable)?;
            Client::open(tokio::io::join(output, input), Some(child), framed).await
        } else {
            #[cfg(unix)]
            let stream = tokio::net::UnixStream::connect(&self.socket)
                .await
                .map_err(|_| BackendError::Unavailable)?;
            #[cfg(windows)]
            let stream = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&self.socket)
                .map_err(|_| BackendError::Unavailable)?;
            Client::open(stream, None, framed).await
        }
    }

    async fn terminals(&self) -> Result<Vec<Terminal>, BackendError> {
        let response = self.call("terminal.list", json!({"all":false})).await?;
        let terminals: Vec<Terminal> = serde_json::from_value(response["terminals"].clone())
            .map_err(|_| BackendError::InvalidResponse("core terminals"))?;
        if terminals.iter().any(|t| !terminal_address(&t.address)) {
            return Err(BackendError::InvalidResponse("core terminal address"));
        }
        Ok(terminals)
    }

    async fn topology(&self) -> Result<Topology, BackendError> {
        let terminals = self.terminals().await?;
        let machines: BTreeSet<_> = terminals.iter().map(|t| t.machine().to_owned()).collect();
        let mut docs = BTreeMap::new();
        for machine in machines {
            let mut params = json!({"name":"herdr-app"});
            if !machine.is_empty() {
                params["on"] = json!(machine);
            }
            // A missing optional layout must not hide otherwise live terminals.
            match self.call("doc.get", params).await {
                Ok(doc) => {
                    docs.insert(machine, doc["value"].clone());
                }
                Err(BackendError::Refused {
                    code: Some(code), ..
                }) if code == "method_not_found" => {}
                Err(error) => return Err(error),
            }
        }
        Ok(project(terminals, &docs))
    }
}

#[derive(Deserialize)]
struct Terminal {
    address: String,
    labels: BTreeMap<String, String>,
    cwd: String,
    cols: u32,
    rows: u32,
    #[serde(default)]
    title: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<Value>,
    #[serde(default)]
    foreground: Option<Value>,
}
impl Terminal {
    fn machine(&self) -> &str {
        self.address
            .rsplit_once('/')
            .map_or("", |(machine, _)| machine)
    }
    fn status(&self) -> AgentStatus {
        AgentStatus::parse(self.status.as_ref().and_then(|s| s["state"].as_str()))
    }
}

fn wire(kind: &str, native: &str) -> String {
    format!("hc{kind}_{}", URL_SAFE_NO_PAD.encode(native))
}
fn native(id: &str, kind: &str) -> Result<String, BackendError> {
    if id.len() > 700 {
        return Err(BackendError::InvalidTarget("core"));
    }
    let encoded = id
        .strip_prefix(&format!("hc{kind}_"))
        .ok_or(BackendError::InvalidTarget("core"))?;
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| BackendError::InvalidTarget("core"))?;
    let value = String::from_utf8(decoded).map_err(|_| BackendError::InvalidTarget("core"))?;
    if value.is_empty()
        || value.len() > 512
        || value.chars().any(char::is_control)
        || wire(kind, &value) != id
        || (kind == "p" && !terminal_address(&value))
    {
        return Err(BackendError::InvalidTarget("core"));
    }
    Ok(value)
}

fn terminal_address(address: &str) -> bool {
    let (machine, terminal) = address.rsplit_once('/').unwrap_or(("", address));
    let Some(number) = terminal.strip_prefix('t') else {
        return false;
    };
    !number.is_empty()
        && number.len() <= 13
        && number
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
        && (number.len() == 1 || !number.starts_with('0'))
        && (machine.is_empty()
            || machine.split('/').all(|part| {
                !part.is_empty()
                    && part.len() <= 64
                    && part.as_bytes()[0].is_ascii_alphanumeric()
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            }))
}
struct Topology {
    workspaces: Vec<Workspace>,
    tabs: Vec<Tab>,
    panes: Vec<Pane>,
}

fn project(terminals: Vec<Terminal>, docs: &BTreeMap<String, Value>) -> Topology {
    let mut workspaces = BTreeMap::<String, Workspace>::new();
    let mut tabs = BTreeMap::<String, Tab>::new();
    let mut panes = Vec::new();
    for terminal in terminals {
        let machine = terminal.machine();
        let space = terminal
            .labels
            .get("herdr.space")
            .unwrap_or(&terminal.address);
        let tab = terminal
            .labels
            .get("herdr.tab")
            .unwrap_or(&terminal.address);
        let workspace_id = wire("w", &format!("{machine}/{space}"));
        let tab_id = wire("t", &format!("{machine}/{space}/{tab}"));
        let pane_id = wire("p", &terminal.address);
        let doc_space = docs
            .get(machine)
            .and_then(|d| d["spaces"].as_array())
            .and_then(|spaces| spaces.iter().find(|s| s["id"].as_str() == Some(space)));
        let label = doc_space
            .and_then(|s| s["name"].as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                terminal
                    .cwd
                    .rsplit(['/', '\\'])
                    .find(|s| !s.is_empty())
                    .unwrap_or(space)
            });
        let label = if machine.is_empty() {
            label.into()
        } else {
            format!("{machine} · {label}")
        };
        let status = terminal.status();
        let workspace = workspaces
            .entry(workspace_id.clone())
            .or_insert_with(|| Workspace {
                id: WorkspaceId::new(&workspace_id),
                number: None,
                label,
                focused: false,
                active_tab_id: Some(TabId::new(&tab_id)),
                tab_count: Some(0),
                pane_count: Some(0),
                agent_status: status,
                repo_root: None,
                // Native paths belong to the core's machine, not necessarily
                // the Gateway host. Do not grant host-local filesystem context.
                checkout_path: None,
            });
        *workspace.pane_count.as_mut().unwrap() += 1;
        let tab_label = doc_space
            .and_then(|s| s["tabs"].as_array())
            .and_then(|tabs| tabs.iter().find(|t| t["id"].as_str() == Some(tab)))
            .and_then(|t| t["name"].as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("1")
            .to_owned();
        if !tabs.contains_key(&tab_id) {
            *workspace.tab_count.as_mut().unwrap() += 1;
        }
        let tab = tabs.entry(tab_id.clone()).or_insert_with(|| Tab {
            id: TabId::new(&tab_id),
            workspace_id: workspace.id.clone(),
            label: tab_label,
            focused: false,
            active_pane_id: Some(PaneId::new(&pane_id)),
            pane_count: Some(0),
        });
        *tab.pane_count.as_mut().unwrap() += 1;
        panes.push(Pane {
            // Core keep can replace the program behind an address. Without a verified
            // incarnation, cross-restart captured-history adoption is forbidden.
            history_identity: None,
            id: PaneId::new(pane_id),
            terminal_id: Some(terminal.address.clone()),
            workspace_id: workspace.id.clone(),
            tab_id: tab.id.clone(),
            label: terminal.name,
            terminal_title: Some(terminal.title),
            cwd: None,
            focused: false,
            width: Some(terminal.cols),
            height: Some(terminal.rows),
            revision: None,
            foreground_command: terminal
                .foreground
                .as_ref()
                .and_then(|v| v["name"].as_str())
                .map(str::to_owned),
            agent: terminal
                .status
                .as_ref()
                .and_then(|s| s["app"].as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            agent_status: status,
            max_offset_from_bottom: None,
            viewport_rows: Some(terminal.rows),
            alternate_on: None,
            cursor_x: None,
            cursor_y: None,
            shell_pid: None,
        });
    }
    Topology {
        workspaces: workspaces.into_values().collect(),
        tabs: tabs.into_values().collect(),
        panes,
    }
}

fn core_layout_activity() -> BackendActivity {
    BackendActivity {
        name: "layout_updated".into(),
        payload: json!({"event":"layout_updated","data":{"backend":"herdr-core"}}),
    }
}
macro_rules! unsupported {
    ($name:ident($($arg:ident: $ty:ty),*) -> $result:ty) => {
        fn $name<'a>(&'a self, $($arg: $ty),*) -> BackendFuture<'a, $result> {
            $(let _ = $arg;)*
            Box::pin(async { Err(BackendError::Unsupported(stringify!($name))) })
        }
    };
}

impl TerminalBackend for HerdrCoreBackend {
    fn metadata(&self) -> BackendFuture<'_, BackendMetadata> {
        Box::pin(async move {
            let hello = self.call("hello", json!({})).await?;
            Ok(BackendMetadata {
                kind: BackendKind::HerdrCore,
                version: hello["version"].as_str().map(str::to_owned),
                protocol: Some(1),
                compatibility_response: None,
            })
        })
    }
    fn activity_stream(&self) -> BackendFuture<'_, BackendActivityStream> {
        let backend = self.clone();
        Box::pin(async move {
            let (mut client, snapshot) = tokio::time::timeout(DEADLINE, async {
                let _slot = RPC_SLOTS
                    .acquire()
                    .await
                    .map_err(|_| BackendError::Unavailable)?;
                let mut client = backend.connect(false).await?;
                let snapshot = client.call("events.subscribe", json!({})).await?;
                Ok::<_, BackendError>((client, snapshot))
            })
            .await
            .map_err(|_| BackendError::Unavailable)??;
            let mut seq = snapshot["seq"]
                .as_u64()
                .ok_or(BackendError::InvalidResponse("core event snapshot"))?;
            let activity = async_stream::try_stream! {
                // Reconnection always invalidates the consumer's old topology.
                yield core_layout_activity();
                loop {
                    let event = client.notification().await?;
                    match event["method"].as_str() {
                        Some("heartbeat") => yield BackendActivity { name: "heartbeat".into(), payload: json!({"event":"heartbeat","data":{}}) },
                        Some("missed") => {
                            seq = event["params"]["seq"].as_u64().ok_or(BackendError::InvalidResponse("core missed snapshot"))?;
                            yield core_layout_activity();
                        }
                        Some("event") => {
                            let next = event["params"]["seq"].as_u64().ok_or(BackendError::InvalidResponse("core event sequence"))?;
                            if next > seq { seq = next; yield core_layout_activity(); }
                        }
                        _ => {}
                    }
                }
            };
            Ok(Box::pin(activity) as BackendActivityStream)
        })
    }
    fn list_workspaces(&self) -> BackendFuture<'_, Vec<Workspace>> {
        Box::pin(async move { Ok(self.topology().await?.workspaces) })
    }
    fn list_tabs(&self) -> BackendFuture<'_, Vec<Tab>> {
        Box::pin(async move { Ok(self.topology().await?.tabs) })
    }
    fn list_panes(&self) -> BackendFuture<'_, Vec<Pane>> {
        Box::pin(async move { Ok(self.topology().await?.panes) })
    }
    fn list_agents(&self) -> BackendFuture<'_, Vec<Agent>> {
        Box::pin(async move {
            Ok(self
                .list_panes()
                .await?
                .into_iter()
                .filter(|p| p.agent.is_some())
                .map(|p| Agent {
                    conversation_id: None,
                    instance_id: None,
                    target: p.id.to_string(),
                    pane_id: p.id,
                    workspace_id: Some(p.workspace_id),
                    tab_id: Some(p.tab_id),
                    kind: p.agent.clone(),
                    display_agent: p.agent,
                    status: p.agent_status,
                    state_change_seq: None,
                    shell_pid: None,
                    pending_user_input: None,
                })
                .collect())
        })
    }
    fn get_pane<'a>(&'a self, id: &'a PaneId) -> BackendFuture<'a, Pane> {
        Box::pin(async move {
            native(id.as_str(), "p")?;
            self.list_panes()
                .await?
                .into_iter()
                .find(|p| p.id == *id)
                .ok_or(BackendError::InvalidTarget("pane"))
        })
    }
    fn get_agent<'a>(&'a self, target: &'a str) -> BackendFuture<'a, Agent> {
        Box::pin(async move {
            native(target, "p")?;
            self.list_agents()
                .await?
                .into_iter()
                .find(|a| a.target == target)
                .ok_or(BackendError::InvalidTarget("agent"))
        })
    }
    fn read_pane<'a>(&'a self, request: &'a ReadPane) -> BackendFuture<'a, PaneOutput> {
        Box::pin(async move {
            let terminal = native(request.pane_id.as_str(), "p")?;
            let response = self
                .call("terminal.read", json!({"terminal":terminal,"lines":0}))
                .await?;
            let text = response["text"]
                .as_str()
                .ok_or(BackendError::InvalidResponse("core screen"))?
                .to_owned();
            Ok(PaneOutput {
                text,
                revision: None,
                range: None,
            })
        })
    }
    unsupported!(create_workspace(request: &'a CreateWorkspace) -> Workspace);
    unsupported!(focus_workspace(id: &'a WorkspaceId) -> ());
    unsupported!(rename_workspace(id: &'a WorkspaceId, label: &'a str) -> ());
    unsupported!(close_workspace(id: &'a WorkspaceId) -> ());
    unsupported!(create_tab(request: &'a CreateTab) -> Tab);
    unsupported!(focus_tab(id: &'a TabId) -> ());
    unsupported!(rename_tab(id: &'a TabId, label: &'a str) -> ());
    unsupported!(close_tab(id: &'a TabId) -> ());
    unsupported!(focus_pane(id: &'a PaneId) -> ());
    unsupported!(rename_pane(id: &'a PaneId, label: &'a str) -> ());
    unsupported!(close_pane(id: &'a PaneId) -> ());
    unsupported!(split_pane(request: &'a SplitPane) -> Pane);
    fn send_text<'a>(
        &'a self,
        id: &'a PaneId,
        text: &'a str,
        mode: SendTextMode,
    ) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let terminal = native(id.as_str(), "p")?;
            tokio::time::timeout(DEADLINE, async {
                let _slot = RPC_SLOTS
                    .acquire()
                    .await
                    .map_err(|_| BackendError::Unavailable)?;
                let mut client = self.connect(true).await?;
                client.input(&terminal, text, mode).await
            })
            .await
            .map_err(|_| BackendError::Unavailable)?
        })
    }
    fn send_keys<'a>(&'a self, id: &'a PaneId, keys: &'a [String]) -> BackendFuture<'a, ()> {
        Box::pin(async move {
            let terminal = native(id.as_str(), "p")?;
            // Parse the whole batch before any input, including aliases and case.
            let keys = keys
                .iter()
                .map(|value| {
                    let mut chord = super::keys::KeyChord::parse(value)
                        .ok_or_else(|| super::keys::unknown_key(value))?;
                    if let super::keys::KeyBase::Char(c) = chord.base {
                        if c.is_ascii_uppercase() {
                            chord.shift = true;
                            chord.base = super::keys::KeyBase::Char(c.to_ascii_lowercase());
                        } else if c == ' ' {
                            chord.base = super::keys::KeyBase::Named(super::keys::NamedKey::Space);
                        } else if !(c.is_ascii_lowercase()
                            || c.is_ascii_digit()
                            || "`\\\\[],=-.';/".contains(c))
                        {
                            return Err(super::keys::unknown_key(value));
                        }
                    }
                    Ok(chord.name())
                })
                .collect::<Result<Vec<_>, BackendError>>()?;
            if keys.is_empty() {
                return Ok(());
            }
            self.call("terminal.keys", json!({"terminal":terminal,"keys":keys}))
                .await?;
            Ok(())
        })
    }
    unsupported!(focus_agent(target: &'a str) -> ());
    unsupported!(prompt_agent(target: &'a str, text: &'a str) -> ());
    unsupported!(start_agent(request: &'a StartAgent) -> StartedAgent);
}
