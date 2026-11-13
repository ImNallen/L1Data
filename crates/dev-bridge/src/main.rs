mod dispatch;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::num::NonZeroU16;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore, ScriptedTransport};
use futures_util::Stream;
use serde_json::{Map, Value, json};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tower_http::cors::CorsLayer;

use dispatch::{BridgeError, dispatch};

struct PortVar {
    name: &'static str,
    default: u16,
}

const BRIDGE_PORT: PortVar = PortVar { name: "L1_DATA_BRIDGE_PORT", default: 7077 };
const VITE_PORT: PortVar = PortVar { name: "L1_DATA_VITE_PORT", default: 5173 };

impl PortVar {
    fn parse(&self, value: Option<&str>) -> u16 {
        let Some(value) = value else { return self.default };
        match value.parse::<NonZeroU16>() {
            Ok(port) if value.bytes().all(|b| b.is_ascii_digit()) => port.get(),
            _ => panic!("{} must be a port from 1 to 65535, got {value:?}", self.name),
        }
    }

    fn read(&self) -> u16 {
        self.parse(std::env::var_os(self.name).map(|v| v.to_string_lossy().into_owned()).as_deref())
    }
}

async fn invoke(State(engine): State<Arc<Engine>>, Path(cmd): Path<String>, body: Bytes) -> Response {
    let args = match parse_args(&body) {
        Ok(args) => args,
        Err(message) => return (StatusCode::BAD_REQUEST, Json(json!({ "message": message }))).into_response(),
    };
    let started = Instant::now();
    let result = dispatch(&engine, &cmd, args).await;
    let elapsed = started.elapsed();
    match result {
        Ok(Ok(data)) => {
            eprintln!("invoke {cmd} -> ok ({elapsed:?})");
            Json(json!({ "status": "ok", "data": data })).into_response()
        }
        Ok(Err(error)) => {
            eprintln!("invoke {cmd} -> error {error} ({elapsed:?})");
            Json(json!({ "status": "error", "error": error })).into_response()
        }
        Err(e) => {
            let (status, message) = match e {
                BridgeError::UnknownCommand(c) => (StatusCode::NOT_FOUND, format!("unknown command `{c}`")),
                BridgeError::BadArg { arg, message } => {
                    (StatusCode::BAD_REQUEST, format!("{cmd}: bad argument `{arg}`: {message}"))
                }
                BridgeError::Panicked(m) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{cmd} panicked: {m}")),
            };
            eprintln!("invoke {cmd} -> {status}: {message}");
            (status, Json(json!({ "message": message }))).into_response()
        }
    }
}

/// curl sends form content-type by default, so the body is parsed as JSON whatever the header says.
fn parse_args(body: &[u8]) -> Result<Map<String, Value>, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match serde_json::from_slice(body) {
        Ok(Value::Object(args)) => Ok(args),
        Ok(_) => Err("args must be a JSON object".into()),
        Err(e) => Err(format!("args are not JSON: {e}")),
    }
}

async fn events(State(engine): State<Arc<Engine>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream =
        BroadcastStream::new(engine.subscribe()).filter_map(|ev| match ev.map(|ev| Event::default().json_data(ev)) {
            Ok(Ok(frame)) => Some(Ok(frame)),
            Ok(Err(e)) => {
                eprintln!("events: failed to encode AppEvent: {e}");
                None
            }
            Err(lagged) => {
                eprintln!("events: {lagged}");
                None
            }
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[tokio::main]
async fn main() {
    let addr = SocketAddr::from(([127, 0, 0, 1], BRIDGE_PORT.read()));
    let vite_port = VITE_PORT.read();
    let temp_dir = tempfile::tempdir().expect("create temp data dir");
    let data_dir = std::env::var_os("L1_DATA_DATA_DIR").map_or_else(|| temp_dir.path().to_owned(), Into::into);
    let scenario = std::env::var("L1_DATA_UPDATE_SCENARIO").unwrap_or_else(|_| "current".into());
    let updates = ScriptedTransport::named(&scenario).unwrap_or_else(|e| panic!("L1_DATA_UPDATE_SCENARIO: {e}"));
    let engine = Engine::open(EngineConfig {
        data_dir,
        keys: Arc::new(MemoryKeyStore::default()),
        agent_path: AgentPath::LoginShell,
        az_binary: std::env::var_os("L1_DATA_AZ_BINARY").map(Into::into),
        user_known_hosts: engine::openssh_known_hosts(),
        updates: Arc::new(updates),
    })
    .await
    .expect("open engine");

    let cors = CorsLayer::new()
        .allow_origin(["localhost", "127.0.0.1"].map(|host| {
            HeaderValue::from_str(&format!("http://{host}:{vite_port}")).expect("vite origin is a valid header value")
        }))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE]);
    let app = Router::new()
        .route("/invoke/{cmd}", post(invoke))
        .route("/events", get(events))
        // Text commands carry the whole document, and Tauri's IPC has no 2 MB cap like axum's default.
        .layer(DefaultBodyLimit::max(64 << 20))
        .layer(cors)
        .with_state(engine.clone());

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind dev-bridge port");
    eprintln!(
        "dev-bridge on http://{addr} for vite on :{vite_port} (data dir {}, update scenario {scenario})",
        engine.data_dir().display()
    );
    // Not a graceful shutdown: that would wait for the `/events` stream, which never ends.
    tokio::select! {
        served = axum::serve(listener, app) => served.expect("serve"),
        _ = tokio::signal::ctrl_c() => eprintln!("dev-bridge: stopping assistant processes"),
    }
    engine.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_port_falls_back_to_its_default() {
        assert_eq!(BRIDGE_PORT.parse(None), 7077);
        assert_eq!(VITE_PORT.parse(None), 5173);
    }

    #[test]
    fn a_set_port_replaces_the_default() {
        assert_eq!(BRIDGE_PORT.parse(Some("7177")), 7177);
        assert_eq!(VITE_PORT.parse(Some("65535")), 65535);
        assert_eq!(BRIDGE_PORT.parse(Some("07177")), 7177);
    }

    #[test]
    fn a_value_that_is_not_a_port_panics_naming_its_variable() {
        for bad in ["abc", "", "0", "65536", "-1", " 7077", "+7077", "7077 ", "７０７７"] {
            let payload = std::panic::catch_unwind(|| BRIDGE_PORT.parse(Some(bad))).expect_err(bad);
            let message = payload.downcast_ref::<String>().expect("formatted panic message");
            assert!(message.starts_with("L1_DATA_BRIDGE_PORT must be a port"), "{bad:?}: {message}");
        }
    }
}
