//! A stand-in for `az account get-access-token`, for tests and the dev bridge.
//!
//! The engine runs it with az's own arguments, so the scenario and log come from the environment
//! the bridge passes down, or from flags a test's wrapper script adds:
//!
//! - `--fake-scenario <name>` or `L1_FAKE_AZ_SCENARIO`. `ok` (the default) prints a token that
//!   expires in an hour. `short-expiry` prints one that expires in 4 minutes, inside the engine's
//!   5-minute refresh margin, so the engine uses it once and runs az again at the next session.
//!   `signed-out` prints az's not-signed-in error and exits 1.
//! - `--fake-log <path>` or `L1_FAKE_AZ_LOG` appends one line per call: unix ms, pid, scenario, and
//!   the arguments as JSON.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let scenario = take(&mut args, "--fake-scenario")
        .or_else(|| std::env::var("L1_FAKE_AZ_SCENARIO").ok())
        .unwrap_or_else(|| "ok".into());
    let log = take(&mut args, "--fake-log").or_else(|| std::env::var("L1_FAKE_AZ_LOG").ok());
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    if let Some(path) = log {
        let line = format!("{}\t{}\t{scenario}\t{}\n", now.as_millis(), std::process::id(), json!(args));
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
    if args.first().map(String::as_str) != Some("account")
        || args.get(1).map(String::as_str) != Some("get-access-token")
    {
        eprintln!("fake-az: only `account get-access-token` is faked, got {args:?}");
        std::process::exit(2);
    }
    let lifetime = match scenario.as_str() {
        "ok" => 3600,
        "short-expiry" => 240,
        "signed-out" => {
            eprintln!("ERROR: Please run 'az login' to setup account.");
            std::process::exit(1);
        }
        other => {
            eprintln!("fake-az: unknown scenario {other}");
            std::process::exit(2);
        }
    };
    let expires_on = now.as_secs() + lifetime;
    let reply = json!({
        "accessToken": format!("fake-az-token-{}", std::process::id()),
        "expiresOn": "2026-10-25 12:00:00.000000",
        "expires_on": expires_on,
        "subscription": "00000000-0000-0000-0000-000000000000",
        "tenant": "00000000-0000-0000-0000-000000000000",
        "tokenType": "Bearer",
    });
    println!("{reply:#}");
}

fn take(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let at = args.iter().position(|a| a == flag)?;
    let value = args.get(at + 1).cloned();
    args.drain(at..(at + 2).min(args.len()));
    value
}
