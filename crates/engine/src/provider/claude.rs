use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::time::Duration;

use serde_json::{Value, json};

use super::agent::Exchange;
use super::{
    AgentEvent, BlockKey, BoxFuture, Codec, Handshake, HostEnv, LaunchSpec, ModelResult, Open, Provider, Quick,
    ReplyTo, SessionStart, Step, agent, capitalize, limits,
};
use crate::Shared;
use crate::model::{
    AgentError, Decision, EffortId, EffortOption, LimitWindow, LimitsOutcome, ModelId, ModelOption, ProbeOutcome,
    ProviderConfig, ProviderInfo, ToolCallId, TurnOutcome,
};

pub(crate) struct Claude;

const SIGN_IN_COMMAND: &str = "claude auth login";

const SERVER: &str = "l1";
const TOOL_PREFIX: &str = "mcp__l1__";
const VERSION_TIMEOUT: Duration = Duration::from_secs(4);
const AUTH_TIMEOUT: Duration = Duration::from_secs(8);

const ENGINE_FLAGS: &[&str] = &[
    "--output-format",
    "--input-format",
    "--verbose",
    "--print",
    "--setting-sources",
    "--settings",
    "--strict-mcp-config",
    "--mcp-config",
    "--plugin-dir",
    "--agents",
    "--no-session-persistence",
    "--tools",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
    "--permission-prompt-tool",
    "--permission-mode",
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--include-partial-messages",
    "--resume",
    "--continue",
    "--session-id",
    "--fork-session",
    "--system-prompt",
    "--system-prompt-file",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--bare",
];

/// What `claude --resume <id>` says, in its `result` frame or on stderr, when it has no
/// transcript for the id.
const SESSION_LOST: &str = "No conversation found";

impl Provider for Claude {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            default_name: "Claude".into(),
            default_binary: "claude".into(),
            config_env: "CLAUDE_CONFIG_DIR".into(),
            config_dir_default: "~/.claude".into(),
            sign_in_command: SIGN_IN_COMMAND.into(),
            owned_flags: ENGINE_FLAGS.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    fn fallback_model(&self) -> (ModelId, Option<EffortId>) {
        (ModelId("opus".into()), Some(EffortId("high".into())))
    }

    fn launch_choice(&self, cfg: &ProviderConfig) -> (Option<ModelId>, Option<EffortId>) {
        let args = parse_launch_args(&cfg.launch_args);
        (args.model.map(ModelId), args.effort.map(EffortId))
    }

    fn ignored_flags(&self, cfg: &ProviderConfig) -> Vec<String> {
        parse_launch_args(&cfg.launch_args).ignored
    }

    fn session_lost(&self, error: &AgentError) -> bool {
        match error {
            AgentError::Provider { message } | AgentError::Exited { detail: message, .. } => {
                message.contains(SESSION_LOST)
            }
            AgentError::NotFound { .. } | AgentError::SignedOut => false,
        }
    }

    fn quick_probe<'a>(
        &'a self,
        cfg: &'a ProviderConfig,
        host: &'a HostEnv,
    ) -> BoxFuture<'a, Result<Quick, ProbeOutcome>> {
        Box::pin(quick_probe(cfg, host))
    }

    fn launch(&self, cfg: &ProviderConfig, host: &HostEnv, start: &SessionStart) -> Result<LaunchSpec, AgentError> {
        let program = host.resolve(&cfg.binary_path, "claude").map_err(|binary| AgentError::NotFound { binary })?;
        let (env_set, env_remove) = env(cfg, host, std::env::vars_os());
        Ok(LaunchSpec {
            program,
            args: argv(start, &parse_launch_args(&cfg.launch_args)),
            env_set,
            env_remove,
            cwd: host.cwd.clone(),
        })
    }

    fn codec(&self, cfg: &ProviderConfig, open: Open) -> Box<dyn Codec> {
        Box::new(ClaudeCodec::new(parse_launch_args(&cfg.launch_args).model, open))
    }
}

async fn quick_probe(cfg: &ProviderConfig, host: &HostEnv) -> Result<Quick, ProbeOutcome> {
    let program = host.resolve(&cfg.binary_path, "claude").map_err(|binary| ProbeOutcome::NotFound { binary })?;
    let (env_set, env_remove) = env(cfg, host, std::env::vars_os());
    let run = |args: &'static [&'static str], limit: Duration| {
        let mut command = tokio::process::Command::new(&program);
        command.args(args).current_dir(&host.cwd).stdin(std::process::Stdio::null()).kill_on_drop(true);
        crate::shell_path::hide_console(&mut command);
        for key in &env_remove {
            command.env_remove(key);
        }
        for (key, value) in &env_set {
            command.env(key, value);
        }
        let what = format!("{} {}", program.display(), args.join(" "));
        async move {
            match tokio::time::timeout(limit, command.output()).await {
                Ok(Ok(output)) => Ok(output),
                Ok(Err(e)) => Err(format!("{what}: {e}")),
                Err(_) => Err(format!("{what} did not answer within {} s", limit.as_secs())),
            }
        }
    };
    let broken = |version: Option<String>, message: String| ProbeOutcome::Broken { version, message };
    let version_out = run(&["--version"], VERSION_TIMEOUT).await.map_err(|m| broken(None, m))?;
    let version_text = String::from_utf8_lossy(&version_out.stdout);
    let version = match (version_out.status.success(), parse_version(&version_text)) {
        (true, Some(version)) => version,
        (true, None) => return Err(broken(None, format!("--version printed {:?}", version_text.trim()))),
        (false, _) => {
            return Err(broken(
                None,
                format!(
                    "--version exited with {}: {}",
                    version_out.status,
                    String::from_utf8_lossy(&version_out.stderr).trim()
                ),
            ));
        }
    };
    // Signed out, the CLI still prints its JSON and exits 1, so the exit code is not read.
    let auth_out = run(&["auth", "status"], AUTH_TIMEOUT).await.map_err(|m| broken(Some(version.clone()), m))?;
    let auth_text = String::from_utf8_lossy(&auth_out.stdout);
    let (signed_in, plan) = parse_auth_status(&auth_text)
        .ok_or_else(|| broken(Some(version.clone()), format!("auth status printed {:?}", auth_text.trim())))?;
    Ok(Quick { version, signed_in, plan })
}

pub(crate) fn parse_version(output: &str) -> Option<String> {
    output
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .map(|w| w.trim_start_matches('v'))
        .find(|w| {
            w.split('.').count() == 3 && w.split('.').all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

pub(crate) fn parse_auth_status(stdout: &str) -> Option<(bool, Option<String>)> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    let logged_in = v["loggedIn"].as_bool()?;
    let plan = plan_label(v["subscriptionType"].as_str(), v["apiProvider"].as_str(), v["authMethod"].as_str());
    Some((logged_in, plan))
}

/// "Claude Max", "Claude Pro", "API key", "Amazon Bedrock". The CLI spells a plan `max` in `auth
/// status` and `Claude Max` in `initialize`, so only the letters are compared.
pub(crate) fn plan_label(
    subscription: Option<&str>,
    api_provider: Option<&str>,
    auth_method: Option<&str>,
) -> Option<String> {
    match api_provider.map(str::to_ascii_lowercase).as_deref() {
        Some("bedrock") => return Some("Amazon Bedrock".into()),
        Some("vertex") => return Some("Google Vertex".into()),
        Some("foundry") => return Some("Microsoft Foundry".into()),
        _ => {}
    }
    if let Some(raw) = subscription.map(str::trim).filter(|s| !s.is_empty()) {
        let key = raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        let key = key.strip_prefix("claude").unwrap_or(&key);
        let key = key.strip_suffix("subscription").unwrap_or(key);
        return Some(match key {
            "max" | "maxplan" => "Claude Max".into(),
            "max5" | "max5x" => "Claude Max 5x".into(),
            "max20" | "max20x" => "Claude Max 20x".into(),
            "pro" => "Claude Pro".into(),
            "team" => "Claude Team".into(),
            "enterprise" => "Claude Enterprise".into(),
            "free" => "Claude Free".into(),
            _ => format!("Claude {raw}"),
        });
    }
    match auth_method {
        Some("none") | Some("claude.ai") | None => None,
        Some(_) => Some("API key".into()),
    }
}

fn effort_label(id: &str) -> String {
    match id {
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" => "Extra high".into(),
        "max" => "Max".into(),
        other => capitalize(other),
    }
}

pub(crate) fn models(raw: &[Value], prefer: Option<&str>) -> Vec<ModelOption> {
    let default_resolved = raw.iter().find(|m| m["value"] == "default").and_then(|m| m["resolvedModel"].as_str());
    let listed: Vec<&Value> = raw.iter().filter(|m| m["value"].as_str().is_some_and(|v| v != "default")).collect();
    let resolving_to = |model: &str| listed.iter().find(|m| m["resolvedModel"] == model);
    let recommended = prefer
        .and_then(|p| listed.iter().find(|m| m["value"] == p).or_else(|| resolving_to(p)))
        .or_else(|| default_resolved.and_then(resolving_to))
        .and_then(|m| m["value"].as_str());
    listed
        .iter()
        .map(|m| {
            let id = m["value"].as_str().unwrap_or_default();
            ModelOption {
                id: ModelId(id.to_owned()),
                label: m["displayName"].as_str().unwrap_or(id).to_owned(),
                description: m["description"].as_str().unwrap_or_default().to_owned(),
                efforts: if m["supportsEffort"].as_bool().unwrap_or(false) {
                    m["supportedEffortLevels"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(|e| EffortOption { id: EffortId(e.to_owned()), label: effort_label(e) })
                        .collect()
                } else {
                    Vec::new()
                },
                recommended: Some(id) == recommended,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LaunchArgs {
    pub passthrough: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub ignored: Vec<String>,
}

/// Whitespace-separated with single or double quotes grouping. `--flag value`, `--flag=value`, and
/// a bare `--flag` are kept; single-dash and positional tokens are dropped, since a positional
/// would become a prompt.
pub(crate) fn parse_launch_args(raw: &str) -> LaunchArgs {
    let tokens = split_args(raw);
    let mut out = LaunchArgs::default();
    let mut i = 0;
    while i < tokens.len() {
        let token = &tokens[i];
        i += 1;
        if !token.starts_with("--") || token == "--" {
            continue;
        }
        let (flag, mut value) = match token.split_once('=') {
            Some((f, v)) => (f.to_owned(), Some(v.to_owned())),
            None => (token.clone(), None),
        };
        if value.is_none() && tokens.get(i).is_some_and(|next| !next.starts_with('-')) {
            value = Some(tokens[i].clone());
            i += 1;
        }
        match flag.as_str() {
            "--model" => out.model = value.filter(|v| !v.is_empty()),
            "--effort" => out.effort = value.filter(|v| !v.is_empty()),
            owned if ENGINE_FLAGS.contains(&owned) => {
                if !out.ignored.contains(&flag) {
                    out.ignored.push(flag);
                }
            }
            _ => {
                out.passthrough.push(flag);
                out.passthrough.extend(value);
            }
        }
    }
    out
}

fn split_args(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => current.push(c),
        }
    }
    if started || !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// `--setting-sources=` drops hooks, plugins, and user settings; `--strict-mcp-config` drops
/// account connectors; `--tools=` removes every built-in tool; `--permission-prompt-tool=stdio`
/// makes permission asks reach the host.
pub(crate) fn argv(start: &SessionStart, args: &LaunchArgs) -> Vec<String> {
    let mut argv: Vec<String> = args.passthrough.clone();
    argv.extend(
        [
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--setting-sources=",
            "--strict-mcp-config",
            "--tools=",
            "--permission-prompt-tool=stdio",
            "--permission-mode=default",
        ]
        .map(str::to_owned),
    );
    if let Some(model) = start.model.as_ref().map(|m| m.0.clone()).or_else(|| args.model.clone()) {
        argv.extend(["--model".into(), model]);
    }
    if let Some(effort) = start.effort.as_ref().map(|e| e.0.clone()).or_else(|| args.effort.clone()) {
        argv.extend(["--effort".into(), effort]);
    }
    argv.push("--include-partial-messages".into());
    if let Some(id) = &start.resume {
        argv.extend(["--resume".into(), id.clone()]);
    }
    argv
}

/// Starts from the engine's env, drops what leaks in from whatever launched the app (another
/// Claude Code session, a different login), and sets what the SDK sets.
pub(crate) fn env(
    cfg: &ProviderConfig,
    host: &HostEnv,
    parent: impl Iterator<Item = (OsString, OsString)>,
) -> (Vec<(OsString, OsString)>, Vec<OsString>) {
    let remove: Vec<OsString> = parent
        .map(|(k, _)| k)
        .filter(|k| {
            let k = k.to_string_lossy();
            k == "CLAUDECODE"
                || k.starts_with("CLAUDE_CODE_")
                || [
                    "CLAUDE_EFFORT",
                    "CLAUDE_AGENT_SDK_VERSION",
                    "NODE_OPTIONS",
                    "ANTHROPIC_API_KEY",
                    "ANTHROPIC_AUTH_TOKEN",
                ]
                .contains(&k.as_ref())
        })
        .collect();
    let mut set: Vec<(OsString, OsString)> = vec![("PATH".into(), host.path.clone())];
    let config_dir = cfg.config_dir.trim();
    if !config_dir.is_empty() {
        set.push(("CLAUDE_CONFIG_DIR".into(), host.expand_home(config_dir).into_os_string()));
    }
    set.extend(
        [
            ("CLAUDE_CODE_ENTRYPOINT", "sdk-ts"),
            ("CLAUDE_CODE_SDK_READS_SESSION_STATE", "1"),
            ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
            ("CLAUDE_CODE_AUTO_CONNECT_IDE", "0"),
            ("CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL", "1"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    );
    (set, remove)
}

enum HostRequest {
    Initialize,
    Interrupt,
}

pub(crate) struct ClaudeCodec {
    next_request: u64,
    pending: HashMap<String, HostRequest>,
    prefer_model: Option<String>,
    open: Open,
    message: u32,
    text_blocks: HashSet<BlockKey>,
    calls_by_rpc_id: HashMap<String, ToolCallId>,
    interrupted: bool,
}

impl ClaudeCodec {
    pub(crate) fn new(prefer_model: Option<String>, open: Open) -> Self {
        Self {
            next_request: 0,
            pending: HashMap::new(),
            prefer_model,
            open,
            message: 0,
            text_blocks: HashSet::new(),
            calls_by_rpc_id: HashMap::new(),
            interrupted: false,
        }
    }

    fn request(&mut self, kind: HostRequest, body: Value) -> String {
        self.next_request += 1;
        let id = format!("req_{}", self.next_request);
        self.pending.insert(id.clone(), kind);
        json!({ "type": "control_request", "request_id": id, "request": body }).to_string()
    }

    fn handshake(&self, response: &Value) -> Handshake {
        let account = &response["account"];
        Handshake {
            version: response["claude_code_version"].as_str().map(str::to_owned),
            plan: plan_label(account["subscriptionType"].as_str(), account["apiProvider"].as_str(), None),
            models: models(response["models"].as_array().map_or(&[][..], Vec::as_slice), self.prefer_model.as_deref()),
        }
    }

    fn mcp(&mut self, request_id: &str, message: &Value) -> Step {
        let rpc_id = message.get("id").cloned().unwrap_or(json!(0));
        let reply = |result: Value| Step {
            replies: vec![mcp_reply(request_id, json!({ "jsonrpc": "2.0", "id": rpc_id, "result": result }))],
            events: Vec::new(),
        };
        match message["method"].as_str() {
            Some("initialize") => reply(json!({
                "protocolVersion": message["params"]["protocolVersion"],
                "capabilities": { "tools": { "listChanged": true } },
                "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
            })),
            Some("tools/list") => reply(json!({
                "tools": self.open.tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": (t.input_schema)() })).collect::<Vec<_>>()
            })),
            Some("tools/call") => {
                let params = &message["params"];
                let call = ToolCallId(
                    params["_meta"]["claudecode/toolUseId"]
                        .as_str()
                        .map_or_else(|| format!("rpc-{rpc_id}"), str::to_owned),
                );
                self.calls_by_rpc_id.insert(rpc_id.to_string(), call.clone());
                Step {
                    replies: Vec::new(),
                    events: vec![AgentEvent::ToolCall {
                        reply: ReplyTo::new(json!({ "request_id": request_id, "id": rpc_id })),
                        call,
                        name: params["name"].as_str().unwrap_or_default().to_owned(),
                        input: params.get("arguments").cloned().unwrap_or(json!({})),
                    }],
                }
            }
            Some("notifications/cancelled") => {
                let cancelled = message["params"]["requestId"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| message["params"]["requestId"].as_u64().map(|n| n.to_string()));
                let mut step = reply(json!({}));
                if let Some(call) = cancelled.and_then(|id| self.calls_by_rpc_id.remove(&id)) {
                    step.events.push(AgentEvent::ToolCancelled { call });
                }
                step
            }
            Some(method) if method.starts_with("notifications/") => reply(json!({})),
            _ => Step {
                replies: vec![mcp_reply(
                    request_id,
                    json!({ "jsonrpc": "2.0", "id": rpc_id, "error": { "code": -32601, "message": "method not found" } }),
                )],
                events: Vec::new(),
            },
        }
    }

    fn result(&self, v: &Value) -> TurnOutcome {
        if self.interrupted {
            return TurnOutcome::Stopped;
        }
        if v["subtype"] == "success" && !v["is_error"].as_bool().unwrap_or(false) {
            return TurnOutcome::Completed;
        }
        let message = v["errors"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .next()
            .map(str::to_owned)
            .or_else(|| v["result"].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
            .unwrap_or_else(|| v["subtype"].as_str().unwrap_or("error").to_owned());
        let signed_out = v["api_error_status"].as_u64() == Some(401)
            || message.contains("/login")
            || message.contains("Not logged in");
        TurnOutcome::Failed { error: if signed_out { AgentError::SignedOut } else { AgentError::Provider { message } } }
    }
}

fn mcp_reply(request_id: &str, rpc: Value) -> String {
    json!({ "type": "control_response", "response": { "subtype": "success", "request_id": request_id, "response": { "mcp_response": rpc } } }).to_string()
}

fn control_error(request_id: &str, error: &str) -> String {
    json!({ "type": "control_response", "response": { "subtype": "error", "request_id": request_id, "error": error } })
        .to_string()
}

fn tool_name(name: &str) -> String {
    name.strip_prefix(TOOL_PREFIX).unwrap_or(name).to_owned()
}

impl Codec for ClaudeCodec {
    fn open(&mut self) -> Vec<String> {
        vec![self.request(
            HostRequest::Initialize,
            json!({ "subtype": "initialize", "systemPrompt": self.open.system_prompt, "sdkMcpServers": [SERVER] }),
        )]
    }

    fn user_turn(&mut self, text: &str) -> Vec<String> {
        vec![json!({ "type": "user", "session_id": "", "message": { "role": "user", "content": [{ "type": "text", "text": text }] }, "parent_tool_use_id": null }).to_string()]
    }

    fn interrupt(&mut self) -> Vec<String> {
        self.interrupted = true;
        vec![self.request(HostRequest::Interrupt, json!({ "subtype": "interrupt" }))]
    }

    fn permission(&mut self, reply: &ReplyTo, decision: Decision, input: &Value, message: &str) -> Vec<String> {
        let (request_id, tool_use_id) = (reply.field("request_id"), reply.field("tool_use_id"));
        let response = match decision {
            Decision::Run => json!({ "behavior": "allow", "updatedInput": input, "toolUseID": tool_use_id }),
            Decision::Deny => json!({ "behavior": "deny", "message": message, "toolUseID": tool_use_id }),
        };
        vec![json!({ "type": "control_response", "response": { "subtype": "success", "request_id": request_id, "response": response } }).to_string()]
    }

    fn tool_result(&mut self, reply: &ReplyTo, result: &ModelResult) -> Vec<String> {
        let rpc_id = reply.field("id");
        self.calls_by_rpc_id.remove(&rpc_id.to_string());
        let mut body = json!({ "content": [{ "type": "text", "text": result.text }] });
        if result.is_error {
            body["isError"] = json!(true);
        }
        vec![mcp_reply(
            reply.field("request_id").as_str().unwrap_or_default(),
            json!({ "jsonrpc": "2.0", "id": rpc_id, "result": body }),
        )]
    }

    fn read(&mut self, line: &str) -> Step {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return Step::default() };
        let event = |event: AgentEvent| Step { replies: Vec::new(), events: vec![event] };
        match v["type"].as_str() {
            Some("control_response") => {
                let response = &v["response"];
                let Some(kind) = response["request_id"].as_str().and_then(|id| self.pending.remove(id)) else {
                    return Step::default();
                };
                match kind {
                    HostRequest::Initialize if response["subtype"] == "success" => {
                        event(AgentEvent::Ready(self.handshake(&response["response"])))
                    }
                    HostRequest::Initialize => {
                        let message = response["error"].as_str().unwrap_or("initialize failed").to_owned();
                        event(AgentEvent::TurnEnded(TurnOutcome::Failed { error: AgentError::Provider { message } }))
                    }
                    HostRequest::Interrupt => Step::default(),
                }
            }
            Some("control_request") => {
                let request_id = v["request_id"].as_str().unwrap_or_default().to_owned();
                let request = &v["request"];
                match request["subtype"].as_str() {
                    Some("mcp_message") if request["server_name"] == SERVER => {
                        self.mcp(&request_id, &request["message"])
                    }
                    Some("can_use_tool") => event(AgentEvent::Permission {
                        reply: ReplyTo::new(json!({ "request_id": request_id, "tool_use_id": request["tool_use_id"] })),
                        call: ToolCallId(request["tool_use_id"].as_str().unwrap_or_default().to_owned()),
                        name: tool_name(request["tool_name"].as_str().unwrap_or_default()),
                        input: request.get("input").cloned().unwrap_or(json!({})),
                    }),
                    _ => Step { replies: vec![control_error(&request_id, "unsupported")], events: Vec::new() },
                }
            }
            Some("system") if v["subtype"] == "init" => {
                event(AgentEvent::TurnStarted { session_id: v["session_id"].as_str().unwrap_or_default().to_owned() })
            }
            Some("stream_event") if v["parent_tool_use_id"].is_null() => {
                let ev = &v["event"];
                let key = |index: &Value| BlockKey { message: self.message, index: index.as_u64().unwrap_or(0) as u32 };
                match ev["type"].as_str() {
                    Some("message_start") => {
                        self.message += 1;
                        Step::default()
                    }
                    Some("content_block_delta") if ev["delta"]["type"] == "text_delta" => {
                        let block = key(&ev["index"]);
                        self.text_blocks.insert(block);
                        event(AgentEvent::Text {
                            block,
                            delta: ev["delta"]["text"].as_str().unwrap_or_default().to_owned(),
                        })
                    }
                    Some("content_block_stop") if self.text_blocks.remove(&key(&ev["index"])) => {
                        event(AgentEvent::TextDone { block: key(&ev["index"]) })
                    }
                    _ => Step::default(),
                }
            }
            Some("assistant") if v["parent_tool_use_id"].is_null() => Step {
                replies: Vec::new(),
                events: v["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|block| block["type"] == "tool_use")
                    .map(|block| AgentEvent::ToolUse {
                        call: ToolCallId(block["id"].as_str().unwrap_or_default().to_owned()),
                        name: tool_name(block["name"].as_str().unwrap_or_default()),
                        input: block.get("input").cloned().unwrap_or(json!({})),
                    })
                    .collect(),
            },
            Some("result") => event(AgentEvent::TurnEnded(self.result(&v))),
            _ => Step::default(),
        }
    }
}

const USAGE_REQUEST_ID: &str = "l1-usage";

pub(crate) struct UsageExchange;

impl Exchange for UsageExchange {
    type Output = Result<ClaudeUsage, String>;

    fn open(&mut self) -> Vec<String> {
        vec![json!({ "type": "control_request", "request_id": USAGE_REQUEST_ID, "request": { "subtype": "get_usage", "skip_behaviors": true } }).to_string()]
    }

    fn read(&mut self, line: &str) -> Option<Self::Output> {
        let v = serde_json::from_str::<Value>(line).ok()?;
        let response = &v["response"];
        if v["type"] != "control_response" || response["request_id"] != USAGE_REQUEST_ID {
            return None;
        }
        Some(if response["subtype"] == "success" {
            parse_usage(&response["response"])
        } else {
            Err(response["error"].as_str().unwrap_or("get_usage failed").to_owned())
        })
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum ClaudeUsage {
    Limited { plan: Option<String>, windows: Vec<LimitWindow> },
    NoRateLimits { plan: Option<String> },
}

pub(crate) fn parse_usage(response: &Value) -> Result<ClaudeUsage, String> {
    let plan = plan_label(response["subscription_type"].as_str(), None, None)
        .map(|p| p.strip_prefix("Claude ").map(str::to_owned).unwrap_or(p));
    let rate_limits = &response["rate_limits"];
    if response["rate_limits_available"] != true || rate_limits.is_null() {
        return Ok(ClaudeUsage::NoRateLimits { plan });
    }
    let entries = rate_limits["limits"].as_array().ok_or("get_usage answered with no rate_limits.limits list")?;
    Ok(ClaudeUsage::Limited { plan, windows: entries.iter().filter_map(usage_window).collect() })
}

fn usage_window(entry: &Value) -> Option<LimitWindow> {
    let left_percent = 100 - entry["percent"].as_f64()?.clamp(0.0, 100.0).round() as u8;
    let kind = entry["kind"].as_str().unwrap_or_default();
    let scope = entry["scope"]["model"]["display_name"].as_str();
    let base = match kind {
        "session" => "Session".to_owned(),
        "weekly_all" | "weekly_scoped" => "Weekly".to_owned(),
        _ => capitalize(entry["group"].as_str().unwrap_or(kind)),
    };
    let (id, label) = match scope {
        Some(scope) => (format!("{kind}:{scope}"), format!("{base} · {scope}")),
        None => (kind.to_owned(), base),
    };
    let resets_at_ms = entry["resets_at"]
        .as_str()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.timestamp_millis());
    Some(LimitWindow {
        id,
        label,
        left_percent,
        resets_at_ms,
        level: limits::level(left_percent, entry["severity"].as_str()),
    })
}

pub(crate) async fn read_limits(shared: &Shared, cfg: &ProviderConfig, host: &HostEnv) -> LimitsOutcome {
    let spec = match Claude.launch(cfg, host, &SessionStart { model: None, effort: None, resume: None }) {
        Ok(spec) => spec,
        Err(AgentError::NotFound { binary }) => return LimitsOutcome::NotFound { binary },
        Err(other) => return limits::start_failed(agent::StartError::Reported(other)),
    };
    let usage = match agent::exchange(shared, &spec, UsageExchange).await {
        Ok(Ok(usage)) => usage,
        Ok(Err(message)) => return LimitsOutcome::Failed { message },
        Err(e) => return limits::start_failed(e),
    };
    let signed_in = match &usage {
        ClaudeUsage::Limited { .. } => None,
        ClaudeUsage::NoRateLimits { .. } => quick_probe(cfg, host).await.ok().map(|quick| quick.signed_in),
    };
    usage_outcome(usage, signed_in)
}

/// `get_usage` answers a signed-out CLI and an API-key sign-in alike, so `auth status` decides.
fn usage_outcome(usage: ClaudeUsage, signed_in: Option<bool>) -> LimitsOutcome {
    match usage {
        ClaudeUsage::Limited { plan, windows } => LimitsOutcome::Ready { plan, windows },
        ClaudeUsage::NoRateLimits { .. } if signed_in == Some(false) => {
            LimitsOutcome::SignedOut { sign_in_command: SIGN_IN_COMMAND.into() }
        }
        ClaudeUsage::NoRateLimits { plan } => LimitsOutcome::NoLimits { plan },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LimitLevel;
    use crate::provider::ToolSpec;

    const INITIALIZE: &str = include_str!("../../tests/fixtures/claude/initialize.jsonl");
    const TURN: &str = include_str!("../../tests/fixtures/claude/turn.jsonl");
    const MCP: &str = include_str!("../../tests/fixtures/claude/mcp.jsonl");
    const PERMISSIONS: &str = include_str!("../../tests/fixtures/claude/permissions.jsonl");
    const INTERRUPT: &str = include_str!("../../tests/fixtures/claude/interrupt.jsonl");
    const RESUME: &str = include_str!("../../tests/fixtures/claude/resume.jsonl");

    fn frames(fixture: &str) -> Vec<Value> {
        fixture
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }

    fn frame(fixture: &str, pick: impl Fn(&Value) -> bool) -> &str {
        fixture
            .lines()
            .find(|l| !l.trim().is_empty() && pick(&serde_json::from_str::<Value>(l).unwrap()))
            .expect("a frame matching the predicate")
    }

    fn json(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    fn opened() -> ClaudeCodec {
        let mut codec = ClaudeCodec::new(None, Open { system_prompt: "You are the assistant.", tools: &TOOLS });
        let lines = codec.open();
        assert_eq!(
            json(&lines[0])["request"],
            json!({ "subtype": "initialize", "systemPrompt": "You are the assistant.", "sdkMcpServers": ["l1"] })
        );
        codec
    }

    static TOOLS: [ToolSpec; 1] = [ToolSpec {
        name: "get_secret_word",
        description: "Returns the secret word. Call this when the user asks for the secret word.",
        input_schema: || json!({ "type": "object", "properties": {}, "additionalProperties": false }),
    }];

    fn initialize_response() -> String {
        let mut v = json(frame(INITIALIZE, |v| v["type"] == "control_response"));
        v["response"]["request_id"] = json!("req_1");
        v.to_string()
    }

    #[test]
    fn the_initialize_response_yields_version_plan_and_models_with_the_default_alias_hidden() {
        let mut codec = opened();
        let step = codec.read(&initialize_response());
        assert!(step.replies.is_empty());
        let [AgentEvent::Ready(handshake)] = step.events.as_slice() else { panic!("{:?}", step.events) };
        assert_eq!(handshake.version.as_deref(), Some("2.1.301"));
        assert_eq!(handshake.plan.as_deref(), Some("Claude Max"));
        let ids: Vec<&str> = handshake.models.iter().map(|m| m.id.0.as_str()).collect();
        assert_eq!(ids[..5], ["opus", "fable", "sonnet", "haiku", "claude-haiku-4-5-20251001"]);
        assert_eq!(handshake.models.len(), 12, "13 captured minus the default alias");
        let recommended: Vec<&str> =
            handshake.models.iter().filter(|m| m.recommended).map(|m| m.id.0.as_str()).collect();
        assert_eq!(recommended, ["opus"]);
        assert_eq!(handshake.models[0].label, "Opus 5.5");
        assert_eq!(
            handshake.models[0].efforts.iter().map(|e| format!("{}={}", e.id.0, e.label)).collect::<Vec<_>>(),
            ["low=Low", "medium=Medium", "high=High", "xhigh=Extra high", "max=Max"]
        );
        assert!(handshake.models[4].efforts.is_empty(), "a model without effort support lists none");
        assert_eq!(handshake.models.iter().find(|m| m.id.0 == "claude-opus-4-6").unwrap().efforts.len(), 4);
        assert_eq!(codec.read(&initialize_response()), Step::default(), "an answer to an unknown request is ignored");
    }

    #[test]
    fn a_launch_argument_model_names_the_recommended_one_by_value_or_by_resolved_model() {
        let recommended_with = |launch_args: &str| {
            let cfg = ProviderConfig {
                launch_args: launch_args.into(),
                ..crate::model::default_config(crate::model::ProviderKind::Claude)
            };
            let mut codec = Claude.codec(&cfg, Open::PROBE);
            codec.open();
            let step = codec.read(&initialize_response());
            let [AgentEvent::Ready(handshake)] = step.events.as_slice() else { panic!("{:?}", step.events) };
            handshake.models.iter().filter(|m| m.recommended).map(|m| m.id.0.clone()).collect::<Vec<_>>()
        };
        assert_eq!(recommended_with("--model claude-opus-5-5"), ["opus"]);
        assert_eq!(
            recommended_with("--model claude-sonnet-5-5"),
            ["sonnet"],
            "a resolved model that is not the default's"
        );
        assert_eq!(recommended_with("--model sonnet"), ["sonnet"]);
        assert_eq!(recommended_with("--model nope"), ["opus"], "an unknown model falls back to the default's model");
    }

    #[test]
    fn owned_flags_are_the_launch_arguments_that_change_nothing_and_are_reported_ignored() {
        let owned = Claude.info().owned_flags;
        for flag in &owned {
            let args = parse_launch_args(&format!("{flag} x"));
            assert_eq!(
                (args.passthrough, args.model, args.effort, args.ignored),
                (vec![], None, None, vec![flag.clone()]),
                "{flag}"
            );
        }
        for flag in [
            "--permission-mode",
            "--plugin-dir",
            "--agents",
            "--append-system-prompt-file",
            "--no-session-persistence",
            "--mcp-config",
            "--settings",
        ] {
            assert!(owned.contains(&flag.to_owned()), "{flag} is not owned");
        }
        assert!(!owned.iter().any(|f| f == "--model" || f == "--effort"), "{owned:?}");
        let cfg = ProviderConfig {
            launch_args: "--verbose --verbose --betas x --settings=/s".into(),
            ..crate::model::default_config(crate::model::ProviderKind::Claude)
        };
        assert_eq!(Claude.ignored_flags(&cfg), ["--verbose", "--settings"], "as typed, each once, in order");
    }

    #[test]
    fn a_lost_session_is_the_no_conversation_found_text_in_the_result_or_on_stderr() {
        let lost = "No conversation found with session ID: 11111111-2222-4333-8444-555555555555";
        assert!(Claude.session_lost(&AgentError::Provider { message: lost.into() }));
        assert!(
            Claude.session_lost(&AgentError::Exited { code: Some(1), detail: format!("warn: something\n{lost}\n") })
        );
        assert!(!Claude.session_lost(&AgentError::Provider { message: "Overloaded".into() }));
        assert!(!Claude.session_lost(&AgentError::Exited { code: Some(1), detail: "killed".into() }));
        assert!(!Claude.session_lost(&AgentError::SignedOut));
    }

    #[test]
    fn the_mcp_handshake_is_answered_by_the_codec_with_the_captured_shapes() {
        let mut codec = opened();
        let frames = frames(MCP);
        let requests: Vec<&Value> = frames.iter().filter(|f| f["type"] == "control_request").collect();
        let answers: Vec<&Value> = frames.iter().filter(|f| f["type"] == "control_response").collect();
        let init = codec.read(&requests[0].to_string());
        assert!(init.events.is_empty());
        let mut mine = json(&init.replies[0]);
        mine["response"]["response"]["mcp_response"]["result"]["serverInfo"]["version"] =
            answers[0]["response"]["response"]["mcp_response"]["result"]["serverInfo"]["version"].clone();
        assert_eq!(mine, *answers[0]);
        let initialized = codec.read(&requests[1].to_string());
        assert_eq!(json(&initialized.replies[0]), *answers[1], "a notification gets the SDK's dummy reply");
        let list = codec.read(&requests[2].to_string());
        assert_eq!(json(&list.replies[0]), *answers[2]);
    }

    #[test]
    fn a_tools_call_carries_the_tool_use_id_and_its_answer_matches_the_captured_one() {
        let mut codec = opened();
        let frames = frames(MCP);
        let call = frames.iter().find(|f| f["request"]["message"]["method"] == "tools/call").unwrap();
        let step = codec.read(&call.to_string());
        assert!(step.replies.is_empty());
        let [AgentEvent::ToolCall { reply, call: id, name, input }] = step.events.as_slice() else {
            panic!("{:?}", step.events)
        };
        assert_eq!(
            (id.0.as_str(), name.as_str(), input),
            ("toolu_01VWceXctBsdrbyvCTabAGKN", "get_secret_word", &json!({}))
        );
        let answer = codec.tool_result(reply, &ModelResult { text: "pineapple".into(), is_error: false });
        let captured = frames
            .iter()
            .find(|f| f["response"]["response"]["mcp_response"]["result"]["content"][0]["text"] == "pineapple")
            .unwrap();
        assert_eq!(json(&answer[0]), *captured);
        let error = codec.tool_result(reply, &ModelResult { text: "boom".into(), is_error: true });
        assert_eq!(json(&error[0])["response"]["response"]["mcp_response"]["result"]["isError"], json!(true));
        let unknown = codec.read(&json!({ "type": "control_request", "request_id": "r9", "request": { "subtype": "mcp_message", "server_name": "l1", "message": { "jsonrpc": "2.0", "id": 7, "method": "resources/list" } } }).to_string());
        assert_eq!(json(&unknown.replies[0])["response"]["response"]["mcp_response"]["error"]["code"], json!(-32601));
    }

    #[test]
    fn a_cancelled_notification_names_the_call_it_cancels() {
        let mut codec = opened();
        let call = frames(MCP).into_iter().find(|f| f["request"]["message"]["method"] == "tools/call").unwrap();
        codec.read(&call.to_string());
        let cancel = json!({ "type": "control_request", "request_id": "c1", "request": { "subtype": "mcp_message", "server_name": "l1", "message": { "jsonrpc": "2.0", "method": "notifications/cancelled", "params": { "requestId": 2 } } } });
        let step = codec.read(&cancel.to_string());
        assert_eq!(
            step.events,
            vec![AgentEvent::ToolCancelled { call: ToolCallId("toolu_01VWceXctBsdrbyvCTabAGKN".into()) }]
        );
        assert_eq!(json(&step.replies[0])["response"]["response"]["mcp_response"]["result"], json!({}));
    }

    #[test]
    fn can_use_tool_becomes_a_permission_event_answered_with_the_captured_allow_and_deny() {
        let mut codec = opened();
        let ask = frame(PERMISSIONS, |v| v["request"]["tool_name"] == "mcp__l1__get_secret_word");
        let step = codec.read(ask);
        let [AgentEvent::Permission { reply, call, name, input }] = step.events.as_slice() else {
            panic!("{:?}", step.events)
        };
        assert_eq!(
            (call.0.as_str(), name.as_str(), input),
            ("toolu_01VWceXctBsdrbyvCTabAGKN", "get_secret_word", &json!({}))
        );
        let allow = codec.permission(reply, Decision::Run, input, "");
        assert_eq!(json(&allow[0]), json(frame(PERMISSIONS, |v| v["response"]["response"]["behavior"] == "allow")));
        let deny = json(&codec.permission(reply, Decision::Deny, input, "The user declined this tool call.")[0]);
        let captured = json(frame(PERMISSIONS, |v| v["response"]["response"]["behavior"] == "deny"));
        assert_eq!(deny["response"]["response"]["message"], captured["response"]["response"]["message"]);
        let keys = |v: &Value| v["response"]["response"].as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(
            keys(&deny),
            keys(&captured),
            "the deny answer has the captured keys; the captured one denied another call"
        );
        let other = codec.read(
            &json!({ "type": "control_request", "request_id": "h1", "request": { "subtype": "hook_callback" } })
                .to_string(),
        );
        assert_eq!(
            json(&other.replies[0])["response"],
            json!({ "subtype": "error", "request_id": "h1", "error": "unsupported" })
        );
    }

    #[test]
    fn a_turn_streams_text_keyed_by_message_and_block_then_ends_completed() {
        let mut codec = opened();
        let mut events = Vec::new();
        for line in TURN.lines().filter(|l| !l.trim().is_empty()) {
            let step = codec.read(line);
            assert!(step.replies.is_empty(), "{line}");
            events.extend(step.events);
        }
        assert_eq!(
            events,
            vec![
                AgentEvent::TurnStarted { session_id: "47f4ae39-701c-42cb-8889-9c74f6359898".into() },
                AgentEvent::Text { block: BlockKey { message: 1, index: 0 }, delta: "391".into() },
                AgentEvent::TextDone { block: BlockKey { message: 1, index: 0 } },
                AgentEvent::TurnEnded(TurnOutcome::Completed),
            ]
        );
        let second = codec.read(frame(TURN, |v| v["event"]["type"] == "message_start"));
        assert!(second.events.is_empty());
        let delta = codec.read(frame(TURN, |v| v["event"]["delta"]["type"] == "text_delta"));
        assert_eq!(
            delta.events,
            vec![AgentEvent::Text { block: BlockKey { message: 2, index: 0 }, delta: "391".into() }],
            "the next message restarts the block index under a new message number"
        );
        let stop_without_text = codec.read(&json!({ "type": "stream_event", "event": { "type": "content_block_stop", "index": 3 }, "parent_tool_use_id": null }).to_string());
        assert_eq!(stop_without_text, Step::default(), "a stop for a block that streamed no text is not a TextDone");
    }

    #[test]
    fn an_assistant_frame_with_a_tool_use_block_becomes_one_tool_use_event() {
        let mut codec = opened();
        let step = codec.read(frame(MCP, |v| v["type"] == "assistant"));
        assert_eq!(
            step.events,
            vec![AgentEvent::ToolUse {
                call: ToolCallId("toolu_01VWceXctBsdrbyvCTabAGKN".into()),
                name: "get_secret_word".into(),
                input: json!({})
            }]
        );
        let text_only = codec.read(frame(TURN, |v| v["type"] == "assistant"));
        assert_eq!(text_only, Step::default());
    }

    #[test]
    fn results_map_to_completed_stopped_signed_out_and_provider_errors() {
        let mut codec = opened();
        let success = frame(TURN, |v| v["type"] == "result");
        assert_eq!(codec.read(success).events, vec![AgentEvent::TurnEnded(TurnOutcome::Completed)]);
        let denied = frame(PERMISSIONS, |v| v["type"] == "result");
        assert_eq!(
            codec.read(denied).events,
            vec![AgentEvent::TurnEnded(TurnOutcome::Completed)],
            "a denied tool is not a failed turn"
        );
        let lost = frame(RESUME, |v| v["type"] == "result" && v["is_error"] == true);
        assert_eq!(
            codec.read(lost).events,
            vec![AgentEvent::TurnEnded(TurnOutcome::Failed {
                error: AgentError::Provider {
                    message: "No conversation found with session ID: 11111111-2222-4333-8444-555555555555".into()
                }
            })]
        );
        let mut unauthorized = json(lost);
        unauthorized["api_error_status"] = json!(401);
        assert_eq!(
            codec.read(&unauthorized.to_string()).events,
            vec![AgentEvent::TurnEnded(TurnOutcome::Failed { error: AgentError::SignedOut })]
        );
        let mut login = json(lost);
        login["errors"] = json!([]);
        login["result"] = json!("Not logged in · Please run /login");
        assert_eq!(
            codec.read(&login.to_string()).events,
            vec![AgentEvent::TurnEnded(TurnOutcome::Failed { error: AgentError::SignedOut })]
        );
    }

    #[test]
    fn an_interrupt_request_matches_the_capture_and_the_aborted_result_is_stopped() {
        let mut codec = opened();
        let lines = codec.interrupt();
        let captured = json(frame(INTERRUPT, |v| v["request"]["subtype"] == "interrupt"));
        assert_eq!(json(&lines[0])["request"], captured["request"]);
        let response = frame(INTERRUPT, |v| v["response"]["response"]["still_queued"].is_array());
        let mut response = json(response);
        response["response"]["request_id"] = json!("req_2");
        assert_eq!(codec.read(&response.to_string()), Step::default());
        assert_eq!(
            codec.read(frame(INTERRUPT, |v| v["type"] == "result")).events,
            vec![AgentEvent::TurnEnded(TurnOutcome::Stopped)]
        );
    }

    #[test]
    fn the_user_turn_matches_the_captured_line() {
        let mut codec = opened();
        let line = codec.user_turn("What is 17 times 23? Think briefly, then reply with only the number.");
        assert_eq!(json(&line[0]), json(frame(TURN, |v| v["type"] == "user")));
    }

    #[test]
    fn unknown_and_unparseable_lines_yield_nothing() {
        let mut codec = opened();
        for line in TURN.lines().chain(INTERRUPT.lines()).chain(RESUME.lines()).filter(|l| !l.trim().is_empty()) {
            let v = json(line);
            let known = ["control_response", "control_request", "result"]
                .contains(&v["type"].as_str().unwrap_or_default())
                || (v["type"] == "system" && v["subtype"] == "init")
                || (v["type"] == "stream_event"
                    && (v["event"]["type"] == "message_start"
                        || v["event"]["delta"]["type"] == "text_delta"
                        || v["event"]["type"] == "content_block_stop"));
            if known {
                continue;
            }
            assert_eq!(codec.read(line), Step::default(), "{line}");
        }
        assert_eq!(codec.read("not json"), Step::default());
        assert_eq!(codec.read(""), Step::default());
        let subagent = json!({ "type": "stream_event", "event": { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "x" } }, "parent_tool_use_id": "toolu_parent" });
        assert_eq!(codec.read(&subagent.to_string()), Step::default(), "a subagent's text is not the reply");
    }

    const USAGE: &str = include_str!("../../tests/fixtures/claude/usage.jsonl");
    const USAGE_SIGNED_OUT: &str = include_str!("../../tests/fixtures/claude/usage-signed-out.jsonl");

    #[test]
    fn the_usage_request_is_one_get_usage_control_request() {
        assert_eq!(
            UsageExchange.open().iter().map(|l| json(l)).collect::<Vec<_>>(),
            [
                json!({ "type": "control_request", "request_id": "l1-usage", "request": { "subtype": "get_usage", "skip_behaviors": true } })
            ]
        );
    }

    #[test]
    fn the_captured_usage_reads_three_windows_from_the_limits_list_alone() {
        let Some(Ok(usage)) = UsageExchange.read(USAGE.trim()) else { panic!("the captured reply is the answer") };
        let window = |id: &str, label: &str, left_percent: u8, resets_at_ms: i64| LimitWindow {
            id: id.into(),
            label: label.into(),
            left_percent,
            resets_at_ms: Some(resets_at_ms),
            level: LimitLevel::Normal,
        };
        assert_eq!(
            usage,
            ClaudeUsage::Limited {
                plan: Some("Max".into()),
                windows: vec![
                    window("session", "Session", 92, 1_792_198_799_595),
                    window("weekly_all", "Weekly", 98, 1_792_774_799_595),
                    window("weekly_scoped:Fable", "Weekly · Fable", 98, 1_792_774_799_595),
                ],
            }
        );
    }

    #[test]
    fn signed_out_the_reply_succeeds_with_no_rate_limits() {
        assert_eq!(UsageExchange.read(USAGE_SIGNED_OUT.trim()), Some(Ok(ClaudeUsage::NoRateLimits { plan: None })));
        let unavailable =
            json!({ "subscription_type": "pro", "rate_limits_available": false, "rate_limits": { "limits": [] } });
        assert_eq!(parse_usage(&unavailable), Ok(ClaudeUsage::NoRateLimits { plan: Some("Pro".into()) }));
        assert_eq!(
            parse_usage(&json!({ "rate_limits_available": true, "rate_limits": {} })),
            Err("get_usage answered with no rate_limits.limits list".into())
        );
    }

    #[test]
    fn no_rate_limits_read_as_signed_out_only_when_auth_status_says_so() {
        let none = || ClaudeUsage::NoRateLimits { plan: None };
        assert_eq!(
            usage_outcome(none(), Some(false)),
            LimitsOutcome::SignedOut { sign_in_command: "claude auth login".into() }
        );
        assert_eq!(usage_outcome(none(), Some(true)), LimitsOutcome::NoLimits { plan: None });
        assert_eq!(
            usage_outcome(none(), None),
            LimitsOutcome::NoLimits { plan: None },
            "an auth status that failed claims nothing"
        );
        let limited = ClaudeUsage::Limited { plan: Some("Max".into()), windows: Vec::new() };
        assert_eq!(
            usage_outcome(limited, None),
            LimitsOutcome::Ready { plan: Some("Max".into()), windows: Vec::new() }
        );
    }

    #[test]
    fn a_window_clamps_used_percent_names_an_unknown_kind_by_group_and_needs_a_percent() {
        let reply = json!({ "subscription_type": "max", "rate_limits_available": true, "rate_limits": { "limits": [
            { "kind": "session", "group": "session", "percent": 140, "severity": "critical", "resets_at": "2026-10-17T01:00:00+00:00", "scope": null },
            { "kind": "monthly_scoped", "group": "monthly", "percent": 85.4, "severity": "warning", "resets_at": null, "scope": { "model": { "display_name": "Opus" } } },
            { "kind": "weekly_all", "group": "weekly", "percent": null, "severity": "normal" },
        ] } });
        let Ok(ClaudeUsage::Limited { windows, .. }) = parse_usage(&reply) else { panic!("the reply has limits") };
        assert_eq!(
            windows,
            [
                LimitWindow {
                    id: "session".into(),
                    label: "Session".into(),
                    left_percent: 0,
                    resets_at_ms: Some(1_792_198_800_000),
                    level: LimitLevel::Exhausted
                },
                LimitWindow {
                    id: "monthly_scoped:Opus".into(),
                    label: "Monthly · Opus".into(),
                    left_percent: 15,
                    resets_at_ms: None,
                    level: LimitLevel::Low
                },
            ]
        );
    }

    #[test]
    fn the_usage_exchange_skips_other_frames_and_reports_an_error_reply() {
        assert_eq!(UsageExchange.read(r#"{"type":"system","subtype":"init"}"#), None);
        assert_eq!(
            UsageExchange.read(
                r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_1","response":{}}}"#
            ),
            None
        );
        assert_eq!(UsageExchange.read("not json"), None);
        assert_eq!(UsageExchange.read(r#"{"type":"control_response","response":{"subtype":"error","request_id":"l1-usage","error":"Unknown subtype"}}"#), Some(Err("Unknown subtype".into())));
    }

    #[test]
    fn versions_and_auth_status_parse_from_the_captured_outputs() {
        assert_eq!(parse_version("2.1.301 (Claude Code)\n").as_deref(), Some("2.1.301"));
        assert_eq!(parse_version("command not found"), None);
        let signed_in = frame(INITIALIZE, |v| v["loggedIn"] == true);
        assert_eq!(parse_auth_status(signed_in), Some((true, Some("Claude Max".into()))));
        let signed_out = frame(INITIALIZE, |v| v["loggedIn"] == false);
        assert_eq!(parse_auth_status(signed_out), Some((false, None)));
        assert_eq!(parse_auth_status("not json"), None);
    }

    #[test]
    fn plan_labels() {
        for (sub, api, auth, label) in [
            (Some("Claude Max"), Some("firstParty"), None, Some("Claude Max")),
            (Some("max"), Some("firstParty"), Some("claude.ai"), Some("Claude Max")),
            (Some("max_5x"), None, None, Some("Claude Max 5x")),
            (Some("pro"), None, None, Some("Claude Pro")),
            (Some("team"), None, None, Some("Claude Team")),
            (Some("enterprise"), None, None, Some("Claude Enterprise")),
            (Some("Some Odd Plan"), None, None, Some("Claude Some Odd Plan")),
            (None, Some("bedrock"), None, Some("Amazon Bedrock")),
            (None, Some("vertex"), None, Some("Google Vertex")),
            (None, Some("firstParty"), Some("api_key"), Some("API key")),
            (None, Some("firstParty"), Some("claude.ai"), None),
            (None, None, Some("none"), None),
        ] {
            assert_eq!(plan_label(sub, api, auth).as_deref(), label, "{sub:?} {api:?} {auth:?}");
        }
    }

    #[test]
    fn launch_arguments_drop_engine_flags_and_lift_model_and_effort() {
        let args = parse_launch_args(
            "--model claude-opus-5-5 --effort=low --debug-file '/tmp/my log' --permission-mode=plan --verbose -p hello --betas x --dangerously-skip-permissions",
        );
        assert_eq!(args.passthrough, ["--debug-file", "/tmp/my log", "--betas", "x"]);
        assert_eq!((args.model.as_deref(), args.effort.as_deref()), (Some("claude-opus-5-5"), Some("low")));
        assert_eq!(args.ignored, ["--permission-mode", "--verbose", "--dangerously-skip-permissions"]);
        assert_eq!(parse_launch_args("  hello -x "), LaunchArgs::default());
        assert_eq!(parse_launch_args("--betas=x --debug").passthrough, ["--betas", "x", "--debug"]);
    }

    #[test]
    fn argv_is_the_verified_isolated_one_plus_model_effort_resume_and_passthrough() {
        let start = SessionStart {
            model: Some(ModelId("opus".into())),
            effort: Some(EffortId("high".into())),
            resume: Some("abc".into()),
        };
        let argv = argv(&start, &parse_launch_args("--model x --debug-file /tmp/l"));
        assert_eq!(
            argv.join(" "),
            "--debug-file /tmp/l --output-format stream-json --verbose --input-format stream-json --setting-sources= --strict-mcp-config --tools= --permission-prompt-tool=stdio --permission-mode=default --model opus --effort high --include-partial-messages --resume abc",
            "the passthrough goes first, so the engine's flags win"
        );
        let probe = argv_for(&SessionStart { model: None, effort: None, resume: None }, "");
        assert!(!probe.iter().any(|a| a == "--model" || a == "--effort" || a == "--resume"), "{probe:?}");
        let preferred = argv_for(&SessionStart { model: None, effort: None, resume: None }, "--model sonnet");
        assert_eq!(
            preferred.iter().position(|a| a == "--model").map(|i| preferred[i + 1].as_str()),
            Some("sonnet"),
            "the probe runs the launch arguments' model"
        );
    }

    fn argv_for(start: &SessionStart, launch_args: &str) -> Vec<String> {
        argv(start, &parse_launch_args(launch_args))
    }

    #[test]
    fn the_child_env_drops_what_leaks_in_and_sets_what_the_sdk_sets() {
        let host = HostEnv {
            path: "/usr/bin:/opt/homebrew/bin".into(),
            home: "/Users/a".into(),
            cwd: "/data/assistant".into(),
        };
        let parent = [
            ("HOME", "/Users/a"),
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "cli"),
            ("CLAUDE_EFFORT", "max"),
            ("ANTHROPIC_API_KEY", "sk"),
            ("ANTHROPIC_AUTH_TOKEN", "t"),
            ("NODE_OPTIONS", "--inspect"),
            ("LANG", "C"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let cfg = ProviderConfig {
            provider: crate::model::ProviderKind::Claude,
            enabled: true,
            display_name: "Claude".into(),
            binary_path: String::new(),
            config_dir: "~/.claude-work".into(),
            launch_args: String::new(),
        };
        let (set, remove) = env(&cfg, &host, parent.into_iter());
        let removed: Vec<String> = remove.iter().map(|k| k.to_string_lossy().into_owned()).collect();
        assert_eq!(
            removed,
            [
                "CLAUDECODE",
                "CLAUDE_CODE_ENTRYPOINT",
                "CLAUDE_EFFORT",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "NODE_OPTIONS"
            ]
        );
        let get = |k: &str| set.iter().find(|(n, _)| n == k).map(|(_, v)| v.to_string_lossy().into_owned());
        assert_eq!(get("PATH").as_deref(), Some("/usr/bin:/opt/homebrew/bin"));
        assert_eq!(get("CLAUDE_CONFIG_DIR").as_deref(), Some("/Users/a/.claude-work"));
        assert_eq!(get("CLAUDE_CODE_ENTRYPOINT").as_deref(), Some("sdk-ts"));
        assert_eq!(get("ENABLE_CLAUDEAI_MCP_SERVERS").as_deref(), Some("false"));
        assert_eq!(get("HOME"), None, "HOME is inherited untouched");
        let inherited = env(&ProviderConfig { config_dir: String::new(), ..cfg }, &host, std::iter::empty()).0;
        assert!(
            !inherited.iter().any(|(k, _)| k == "CLAUDE_CONFIG_DIR"),
            "an empty config dir leaves the variable to the environment"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_binary_resolves_on_the_host_path_or_names_where_it_looked() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude = bin.join("claude");
        std::fs::write(&claude, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = HostEnv {
            path: std::env::join_paths([bin.clone(), std::path::PathBuf::from("/nonexistent")]).unwrap(),
            home: dir.path().to_owned(),
            cwd: dir.path().join("assistant"),
        };
        assert_eq!(host.resolve("", "claude").unwrap(), claude);
        assert_eq!(host.resolve("~/bin/claude", "claude").unwrap(), claude);
        assert_eq!(host.resolve("/nope", "claude").unwrap_err(), "/nope");
        assert_eq!(
            host.resolve("missing", "claude").unwrap_err(),
            format!("missing on PATH ({}:/nonexistent)", bin.display())
        );
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            host.resolve("nope/claude", "claude").unwrap_err(),
            here.join("nope/claude").display().to_string(),
            "a relative path is taken against the engine's own directory"
        );
    }
}
