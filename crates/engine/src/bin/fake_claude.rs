use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const VERSION: &str = "2.1.301 (Claude Code)";
const CARD_REPLY: &str = "This compares paid revenue in the last 90 days with the 90 days before that, per region:\n\n```sql\nSELECT c.region,\n  sum(o.total) FILTER (\n    WHERE o.created_at >= now() - interval '90 days'\n  ) - sum(o.total) FILTER (\n    WHERE o.created_at <  now() - interval '90 days'\n  ) AS growth\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'paid'\n  AND o.created_at >= now() - interval '180 days'\nGROUP BY c.region\nORDER BY growth DESC;\n```\n\n`FILTER` keeps both periods in one scan of `orders`, which already has an index on `created_at`.\n\n```followups\nShow growth as a percentage\nOnly EU regions\n```\n";
const DELTA_CHARS: usize = 12;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut log = Log::open(&args);
    log.write("argv", &format!("{}\t{}", std::process::id(), json!(args)));
    if args == ["--version"] {
        println!("{VERSION}");
        return;
    }
    let signed_out = std::env::var("CLAUDE_CONFIG_DIR").is_ok_and(|d| d.trim_end_matches('/').ends_with("signed-out"));
    if args == ["auth", "status"] {
        let status = if signed_out {
            json!({ "loggedIn": false, "authMethod": "none", "apiProvider": "firstParty" })
        } else {
            json!({ "loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty", "email": "user@example.com", "subscriptionType": "max" })
        };
        println!("{status}");
        std::process::exit(if signed_out { 1 } else { 0 });
    }
    let value_of = |flag: &str| args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone());
    let scenario = value_of("--fake-scenario").unwrap_or_default();
    let resume = value_of("--resume");
    match (scenario.as_str(), &resume) {
        ("resume-lost" | "resume-lost-twice", Some(id)) => session_lost(id),
        ("resume-crash", Some(_)) => {
            eprintln!("fake-claude: boom");
            std::process::exit(1);
        }
        ("resume-lost-twice", None) => {
            eprintln!("fake-claude: no session store");
            std::process::exit(1);
        }
        _ => {}
    }
    let resumed = resume.is_some();
    let session_id = resume.unwrap_or_else(|| format!("fake-session-{}", std::process::id()));
    let model = value_of("--model").unwrap_or_else(|| "claude-opus-5-5".into());
    let sql = value_of("--fake-sql").unwrap_or_else(|| "select count(*) from orders".into());
    let linger = Duration::from_millis(value_of("--fake-linger-ms").and_then(|v| v.parse().ok()).unwrap_or(0));
    let signed_out = signed_out || scenario == "signed-out-start";
    let mut cli = Cli {
        stdin: stdin_lines(),
        backlog: VecDeque::new(),
        log,
        session_id,
        resumed,
        model,
        sql,
        linger,
        signed_out,
        next_id: 0,
    };
    if scenario == "signed-out-start" {
        cli.read_line();
        println!(
            "{}",
            json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "num_turns": 0, "session_id": "", "errors": [], "result": "Not logged in · Please run /login", "result_index": 0 })
        );
        std::process::exit(1);
    }
    cli.handshake();
    cli.run(&scenario);
}

fn usage(request_id: &Value, signed_out: bool) -> Value {
    let reply = if signed_out {
        json!({ "subscription_type": null, "rate_limits_available": false, "rate_limits": null, "behaviors": null })
    } else {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        let at = |secs: i64| {
            let t = chrono::DateTime::from_timestamp(now + secs, 0).unwrap_or_default().naive_utc();
            format!("{}T{}+00:00", t.date(), t.time())
        };
        let (session, weekly) = (at(86 * 60), at((3 * 24 + 4) * 3600));
        json!({ "subscription_type": "max", "rate_limits_available": true, "rate_limits": {
            "five_hour": { "utilization": 90, "resets_at": session }, "seven_day": { "utilization": 60, "resets_at": weekly }, "seven_day_opus": null, "iguana_necktie": null,
            "limits": [
                { "kind": "session", "group": "session", "percent": 90, "severity": "warning", "resets_at": session, "scope": null, "is_active": true },
                { "kind": "weekly_all", "group": "weekly", "percent": 60, "severity": "normal", "resets_at": weekly, "scope": null, "is_active": false },
                { "kind": "weekly_scoped", "group": "weekly", "percent": 62, "severity": "normal", "resets_at": weekly, "scope": { "model": { "id": null, "display_name": "Opus" }, "surface": null }, "is_active": false },
            ],
        }, "behaviors": null })
    };
    json!({ "type": "control_response", "response": { "subtype": "success", "request_id": request_id, "response": reply } })
}

fn session_lost(id: &str) -> ! {
    println!(
        "{}",
        json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "num_turns": 0, "session_id": id, "errors": [format!("No conversation found with session ID: {id}")], "result_index": 0 })
    );
    eprintln!("No conversation found with session ID: {id}");
    std::process::exit(1)
}

struct Log(Option<std::fs::File>);

impl Log {
    fn open(args: &[String]) -> Self {
        let path = args
            .windows(2)
            .find(|w| w[0] == "--fake-log")
            .map(|w| w[1].clone())
            .or_else(|| std::env::var("L1_FAKE_CLAUDE_LOG").ok());
        Self(path.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok()))
    }

    /// One write call per line, so lines from the probe's process and a turn's never interleave.
    fn write(&mut self, kind: &str, text: &str) {
        if let Some(file) = self.0.as_mut() {
            let _ = file.write_all(format!("{kind}\t{text}\n").as_bytes());
        }
    }
}

enum Stdin {
    Line(String),
    Eof,
}

fn stdin_lines() -> mpsc::Receiver<Stdin> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            if tx.send(Stdin::Line(line)).is_err() {
                return;
            }
        }
        let _ = tx.send(Stdin::Eof);
    });
    rx
}

struct Cli {
    stdin: mpsc::Receiver<Stdin>,
    backlog: VecDeque<Value>,
    log: Log,
    session_id: String,
    resumed: bool,
    model: String,
    sql: String,
    linger: Duration,
    signed_out: bool,
    next_id: u64,
}

impl Cli {
    fn send(&mut self, frame: Value) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{frame}");
        let _ = out.flush();
    }

    fn parse_or_answer_usage(&mut self, line: String) -> Option<Value> {
        self.log.write("stdin", &line);
        let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
        if v["request"]["subtype"] == "get_usage" {
            self.send(usage(&v["request_id"], self.signed_out));
            return None;
        }
        Some(v)
    }

    fn poll_line(&mut self, wait: Duration) -> Option<Value> {
        if let Some(v) = self.backlog.pop_front() {
            return Some(v);
        }
        match self.stdin.recv_timeout(wait) {
            Ok(Stdin::Line(line)) => self.parse_or_answer_usage(line),
            Ok(Stdin::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => self.exit_on_eof(),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
        }
    }

    fn exit_on_eof(&mut self) -> ! {
        std::thread::sleep(self.linger);
        self.log.write("exit", &std::process::id().to_string());
        std::process::exit(0)
    }

    fn expect(&mut self, what: &str, pick: impl Fn(&Value) -> bool) -> Value {
        if let Some(at) = self.backlog.iter().position(&pick) {
            return self.backlog.remove(at).expect("found above");
        }
        loop {
            let v = self.read_line();
            if pick(&v) {
                return v;
            }
            self.log.write("queued", &format!("while waiting for {what}: {v}"));
            self.backlog.push_back(v);
        }
    }

    fn read_line(&mut self) -> Value {
        loop {
            match self.stdin.recv() {
                Ok(Stdin::Line(line)) => {
                    if let Some(v) = self.parse_or_answer_usage(line) {
                        return v;
                    }
                }
                Ok(Stdin::Eof) | Err(_) => self.exit_on_eof(),
            }
        }
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn mcp(&mut self, message: Value) -> Value {
        let id = format!("fake-req-{}", self.next_id());
        self.send(json!({ "type": "control_request", "request_id": id, "request": { "subtype": "mcp_message", "server_name": "l1", "message": message } }));
        self.expect("an MCP answer", |v| v["response"]["request_id"] == id)
    }

    fn handshake(&mut self) {
        let init = self.expect("initialize", |v| v["request"]["subtype"] == "initialize");
        let models = ["opus", "fable", "sonnet", "haiku"].map(|value| {
            json!({ "value": value, "resolvedModel": format!("claude-{value}-5-5"), "displayName": format!("{} 5.5", capitalize(value)), "description": "Fake", "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"] })
        });
        let mut list = vec![
            json!({ "value": "default", "resolvedModel": "claude-opus-5-5", "displayName": "Default (recommended)", "description": "Opus 5.5", "supportsEffort": true, "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"] }),
        ];
        list.extend(models);
        list.push(json!({ "value": "fake", "resolvedModel": "fake-1-0", "displayName": "Fake 1.0", "description": "Only the stand-in lists this model", "supportsThinkingOff": true }));
        self.send(json!({ "type": "control_response", "response": { "subtype": "success", "request_id": init["request_id"], "response": {
            "commands": [], "agents": [], "output_style": "default", "available_output_styles": ["default"],
            "models": list,
            "account": { "email": "user@example.com", "organization": "user@example.com's Organization", "subscriptionType": "Claude Max", "apiProvider": "firstParty" },
            "pid": std::process::id(), "claude_code_version": "2.1.301", "current_permission_mode": "default", "session_state": "idle", "capabilities": []
        } } }));
        self.mcp(json!({ "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "claude-code", "version": "2.1.301" } }, "jsonrpc": "2.0", "id": 0 }));
        self.mcp(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        self.mcp(json!({ "method": "tools/list", "jsonrpc": "2.0", "id": 1 }));
    }

    fn run(&mut self, scenario: &str) {
        loop {
            let user = self.expect("a user message", |v| v["type"] == "user");
            let prompt = user["message"]["content"][0]["text"].as_str().unwrap_or_default().to_owned();
            if scenario == "resume-lost-late" && self.resumed {
                session_lost(&self.session_id);
            }
            self.turn_start();
            match scenario {
                "card" => {
                    self.stream(CARD_REPLY, Duration::from_millis(30));
                    self.result_success();
                }
                "slow" => self.slow(false),
                "stop-then-query" => self.slow(true),
                "wedged" => {
                    self.message_start();
                    loop {
                        self.delta("tick ");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                "crash" => {
                    self.message_start();
                    self.delta("Working");
                    self.delta(" on it");
                    eprintln!("fake-claude: simulated crash");
                    std::process::exit(1);
                }
                "query" => {
                    let input = json!({ "sql": self.sql, "purpose": "Count the orders" });
                    self.tool_turn("run_query", input, None, None);
                }
                "query-forged" => {
                    let input = json!({ "sql": self.sql, "purpose": "Direct query without permission" });
                    let id = self.tool_use("run_query", &input);
                    let (text, _) = self.call("run_query", &id, input);
                    self.stream(&text, Duration::ZERO);
                    self.result_success();
                }
                "query-delayed" => {
                    let input = json!({ "sql": self.sql, "purpose": "Query after approval" });
                    let id = self.tool_use("run_query", &input);
                    self.ask("run_query", &id, &input);
                    self.log.write("approved", &id);
                    std::thread::sleep(Duration::from_millis(750));
                    self.call("run_query", &id, input);
                    self.result_success();
                }
                "search" => self.tool_turn("search_catalog", json!({ "query": "orders" }), None, None),
                "query-tampered" => self.tool_turn(
                    "run_query",
                    json!({ "sql": "select count(*) from orders", "purpose": "Count the orders" }),
                    None,
                    Some(json!({ "sql": "select count(*) from customers", "purpose": "Count the orders" })),
                ),
                "ask-differs" => self.tool_turn(
                    "run_query",
                    json!({ "sql": "select count(*) from orders", "purpose": "Count the orders" }),
                    Some(json!({ "sql": "select count(*) from customers", "purpose": "Count the orders" })),
                    None,
                ),
                "call-before-answer" => {
                    let asked = json!({ "sql": "select count(*) from orders", "purpose": "Count the orders" });
                    let tool_use_id = self.tool_use("run_query", &asked);
                    let ask_id = self.send_ask("run_query", &tool_use_id, &asked);
                    let (text, failed) = self.call(
                        "run_query",
                        &tool_use_id,
                        json!({ "sql": "select count(*) from customers", "purpose": "Count the orders" }),
                    );
                    let decision = self.answer_to(&ask_id);
                    let outcome =
                        if failed { format!("The tool failed: {text}") } else { format!("The tool returned:\n{text}") };
                    let reply = format!(
                        "{outcome}; the ask was {}",
                        if decision["behavior"] == "allow" { "allowed" } else { "denied" }
                    );
                    self.stream(&reply, Duration::ZERO);
                    self.result_success();
                }
                "describe" => self.tool_turn("describe_tables", json!({ "tables": ["orders"] }), None, None),
                "echo" => {
                    self.stream(&prompt, Duration::ZERO);
                    self.result_success();
                }
                _ => {
                    self.stream("ok", Duration::ZERO);
                    self.result_success();
                }
            }
        }
    }

    fn turn_start(&mut self) {
        self.send(json!({ "type": "system", "subtype": "session_state_changed", "state": "running", "session_id": self.session_id }));
        self.send(json!({ "type": "system", "subtype": "init", "cwd": std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(), "session_id": self.session_id, "tools": ["mcp__l1__describe_tables", "mcp__l1__search_catalog", "mcp__l1__run_query"], "mcp_servers": [{ "name": "l1", "status": "connected", "source": "sdk" }], "model": self.model, "permissionMode": "default", "claude_code_version": "2.1.301" }));
        self.send(
            json!({ "type": "system", "subtype": "status", "status": "requesting", "session_id": self.session_id }),
        );
    }

    fn message_start(&mut self) {
        let id = format!("msg_fake_{}", self.next_id());
        self.send(json!({ "type": "stream_event", "event": { "type": "message_start", "message": { "model": self.model, "id": id, "type": "message", "role": "assistant", "content": [], "stop_reason": null } }, "session_id": self.session_id, "parent_tool_use_id": null }));
        self.send(json!({ "type": "stream_event", "event": { "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }, "session_id": self.session_id, "parent_tool_use_id": null }));
    }

    fn delta(&mut self, text: &str) {
        self.send(json!({ "type": "stream_event", "event": { "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": text } }, "session_id": self.session_id, "parent_tool_use_id": null }));
    }

    fn message_end(&mut self, text: &str, stop_reason: &str) {
        self.send(json!({ "type": "assistant", "message": { "model": self.model, "role": "assistant", "content": [{ "type": "text", "text": text }], "stop_reason": null }, "parent_tool_use_id": null, "session_id": self.session_id }));
        self.send(json!({ "type": "stream_event", "event": { "type": "content_block_stop", "index": 0 }, "session_id": self.session_id, "parent_tool_use_id": null }));
        self.send(json!({ "type": "stream_event", "event": { "type": "message_delta", "delta": { "stop_reason": stop_reason, "stop_sequence": null } }, "session_id": self.session_id, "parent_tool_use_id": null }));
        self.send(json!({ "type": "stream_event", "event": { "type": "message_stop" }, "session_id": self.session_id, "parent_tool_use_id": null }));
    }

    fn stream(&mut self, text: &str, gap: Duration) {
        self.message_start();
        let chars: Vec<char> = text.chars().collect();
        for chunk in chars.chunks(DELTA_CHARS) {
            self.delta(&chunk.iter().collect::<String>());
            if !gap.is_zero() {
                std::thread::sleep(gap);
            }
        }
        self.message_end(text, "end_turn");
    }

    fn result_success(&mut self) {
        self.send(json!({ "type": "result", "subtype": "success", "is_error": false, "num_turns": 1, "duration_ms": 10, "result": "", "session_id": self.session_id, "permission_denials": [], "terminal_reason": "completed", "api_error_status": null }));
        self.send(json!({ "type": "system", "subtype": "session_state_changed", "state": "idle", "session_id": self.session_id }));
    }

    fn slow(&mut self, query_on_interrupt: bool) {
        self.message_start();
        let started = Instant::now();
        let mut text = String::new();
        let mut n = 0;
        while started.elapsed() < Duration::from_secs(60) {
            n += 1;
            let piece = format!("tick {n} ");
            text.push_str(&piece);
            self.delta(&piece);
            if let Some(v) = self.poll_line(Duration::from_millis(100))
                && v["request"]["subtype"] == "interrupt"
            {
                self.send(json!({ "type": "control_response", "response": { "subtype": "success", "request_id": v["request_id"], "response": { "still_queued": [] } } }));
                if query_on_interrupt {
                    let input = json!({ "sql": "select count(*) from orders", "purpose": "Count the orders" });
                    let tool_use_id = self.tool_use("run_query", &input);
                    self.ask("run_query", &tool_use_id, &input);
                    self.call("run_query", &tool_use_id, input);
                }
                self.send(json!({ "type": "assistant", "message": { "model": self.model, "role": "assistant", "content": [{ "type": "text", "text": text }], "stop_reason": null }, "parent_tool_use_id": null, "session_id": self.session_id, "aborted": true }));
                self.send(json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": "[Request interrupted by user]" }] }, "parent_tool_use_id": null, "session_id": self.session_id }));
                self.send(json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "num_turns": 1, "session_id": self.session_id, "permission_denials": [], "terminal_reason": "aborted_streaming", "errors": ["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"] }));
                self.send(json!({ "type": "system", "subtype": "session_state_changed", "state": "idle", "session_id": self.session_id }));
                return;
            }
        }
        self.message_end(&text, "end_turn");
        self.result_success();
    }

    fn tool_turn(&mut self, tool: &str, input: Value, ask_with: Option<Value>, call_with: Option<Value>) {
        let tool_use_id = self.tool_use(tool, &input);
        let decision = self.ask(tool, &tool_use_id, &ask_with.unwrap_or_else(|| input.clone()));
        let reply = if decision["behavior"] == "allow" {
            let arguments = call_with.unwrap_or_else(|| decision.get("updatedInput").cloned().unwrap_or(input));
            let (text, failed) = self.call(tool, &tool_use_id, arguments);
            if failed { format!("The tool failed: {text}") } else { format!("The tool returned:\n{text}") }
        } else {
            let message = decision["message"].as_str().unwrap_or_default();
            self.send(json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "content": message, "is_error": true, "tool_use_id": tool_use_id }] }, "parent_tool_use_id": null, "session_id": self.session_id }));
            format!("The call was declined: {message}")
        };
        self.stream(&reply, Duration::ZERO);
        self.result_success();
    }

    fn tool_use(&mut self, tool: &str, input: &Value) -> String {
        let tool_use_id = format!("toolu_fake_{}", self.next_id());
        let full_name = format!("mcp__l1__{tool}");
        let msg_id = format!("msg_fake_{}", self.next_id());
        self.send(json!({ "type": "stream_event", "event": { "type": "message_start", "message": { "model": self.model, "id": msg_id, "type": "message", "role": "assistant", "content": [], "stop_reason": null } }, "session_id": self.session_id, "parent_tool_use_id": null }));
        self.send(json!({ "type": "stream_event", "event": { "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": tool_use_id, "name": full_name, "input": {} } }, "session_id": self.session_id, "parent_tool_use_id": null }));
        self.send(json!({ "type": "assistant", "message": { "model": self.model, "role": "assistant", "content": [{ "type": "tool_use", "id": tool_use_id, "name": full_name, "input": input }], "stop_reason": null }, "parent_tool_use_id": null, "session_id": self.session_id }));
        self.send(json!({ "type": "stream_event", "event": { "type": "content_block_stop", "index": 0 }, "session_id": self.session_id, "parent_tool_use_id": null }));
        self.send(json!({ "type": "stream_event", "event": { "type": "message_delta", "delta": { "stop_reason": "tool_use" } }, "session_id": self.session_id, "parent_tool_use_id": null }));
        self.send(json!({ "type": "stream_event", "event": { "type": "message_stop" }, "session_id": self.session_id, "parent_tool_use_id": null }));
        tool_use_id
    }

    fn ask(&mut self, tool: &str, tool_use_id: &str, input: &Value) -> Value {
        let ask_id = self.send_ask(tool, tool_use_id, input);
        self.answer_to(&ask_id)
    }

    fn send_ask(&mut self, tool: &str, tool_use_id: &str, input: &Value) -> String {
        let ask_id = format!("fake-ask-{}", self.next_id());
        self.send(json!({ "type": "control_request", "request_id": ask_id, "request": { "subtype": "can_use_tool", "tool_name": format!("mcp__l1__{tool}"), "mcp_server": { "name": "l1", "source": "sdk" }, "display_name": tool, "input": input, "permission_suggestions": [], "tool_use_id": tool_use_id } }));
        ask_id
    }

    fn answer_to(&mut self, ask_id: &str) -> Value {
        let answer = self.expect("the permission answer", |v| v["response"]["request_id"] == ask_id);
        answer["response"]["response"].clone()
    }

    fn call(&mut self, tool: &str, tool_use_id: &str, arguments: Value) -> (String, bool) {
        let call = self.mcp(json!({ "method": "tools/call", "params": { "name": tool, "arguments": arguments, "_meta": { "claudecode/toolUseId": tool_use_id, "progressToken": 2 } }, "jsonrpc": "2.0", "id": 2 }));
        let result = &call["response"]["response"]["mcp_response"]["result"];
        let text = result["content"][0]["text"].as_str().unwrap_or_default().to_owned();
        let failed = result["isError"] == true;
        self.send(json!({ "type": "user", "message": { "role": "user", "content": [{ "tool_use_id": tool_use_id, "type": "tool_result", "content": text, "is_error": failed }] }, "parent_tool_use_id": null, "session_id": self.session_id }));
        (text, failed)
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}
