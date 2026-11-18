use engine::model::{ConnectionDraft, DatabaseKind, PasswordInput, SslMode};
use engine::{AgentPath, Engine, EngineConfig, MemoryKeyStore};
use std::{path::PathBuf, sync::Arc};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 5, "expected data directory, host, port, mode");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let engine = Engine::open(EngineConfig {
            data_dir: PathBuf::from(&args[1]),
            keys: Arc::new(MemoryKeyStore::default()),
            agent_path: AgentPath::Fixed("".into()),
            az_binary: None,
            user_known_hosts: None,
            updates: Arc::new(engine::ScriptedTransport::named("off").unwrap()),
        })
        .await
        .unwrap();
        let result = engine
            .test_connection(ConnectionDraft {
                database_kind: DatabaseKind::SqlServer,
                id: None,
                name: "disposable TLS fixture".into(),
                host: args[2].clone(),
                port: args[3].parse().unwrap(),
                database: "master".into(),
                user: "sa".into(),
                password: PasswordInput::Set(std::env::var("MSSQL_SA_PASSWORD").unwrap()),
                ssl_mode: match args[4].as_str() {
                    "VerifyFull" => SslMode::VerifyFull,
                    "Require" => SslMode::Require,
                    _ => panic!("unsupported probe mode"),
                },
                ca_cert_path: None,
                color: None,
                ssh: None,
                environment: None,
                sql_server_auth: None,
                assistant_read_only: false,
            })
            .await;
        println!("{}", serde_json::to_string(&result).unwrap());
        engine.shutdown().await;
    });
}
