use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::client::{AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey};
use russh::{MethodKind, MethodSet};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{Client, TunnelSpec};
use crate::model::{SshAuth, SshError};

pub(super) async fn authenticate(handle: &mut Handle<Client>, spec: &TunnelSpec) -> Result<(), SshError> {
    match &spec.auth {
        SshAuth::Password => password(handle, spec, spec.secret().ok_or(SshError::MissingSecret)?).await,
        SshAuth::Key { path } => key(handle, spec, &expand_home(path)).await,
        SshAuth::Agent { socket } => agent(handle, spec, socket.as_deref()).await,
    }
}

fn network(e: russh::Error) -> SshError {
    SshError::Network { message: e.to_string() }
}

fn refused(spec: &TunnelSpec, method: &str, remaining: &MethodSet) -> SshError {
    let accepted: Vec<&str> = remaining.iter().map(<&str>::from).collect();
    let accepted = if accepted.is_empty() { "no other method".to_owned() } else { accepted.join(", ") };
    SshError::Auth {
        message: format!("{}:{} refused {method} for {}. It accepts {accepted}.", spec.host, spec.port, spec.user),
    }
}

fn accepted(result: AuthResult, spec: &TunnelSpec, method: &str) -> Result<(), SshError> {
    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Failure { remaining_methods, partial_success: true } => {
            Err(refused(spec, &format!("{method} alone"), &remaining_methods))
        }
        AuthResult::Failure { remaining_methods, .. } => Err(refused(spec, method, &remaining_methods)),
    }
}

/// A server that takes only keyboard-interactive gets the password as the answer to one hidden
/// prompt. Anything more is a second factor this tunnel cannot answer.
async fn password(handle: &mut Handle<Client>, spec: &TunnelSpec, password: &str) -> Result<(), SshError> {
    let result = handle.authenticate_password(&spec.user, password).await.map_err(network)?;
    match &result {
        AuthResult::Failure { remaining_methods, partial_success: false }
            if !remaining_methods.contains(&MethodKind::Password)
                && remaining_methods.contains(&MethodKind::KeyboardInteractive) => {}
        _ => return accepted(result, spec, "the password"),
    }
    let mut answered = false;
    let mut response = handle.authenticate_keyboard_interactive_start(&spec.user, None).await.map_err(network)?;
    loop {
        let prompts = match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure { remaining_methods, .. } => {
                return Err(refused(spec, "the password", &remaining_methods));
            }
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => prompts,
        };
        let answers = match prompts.as_slice() {
            [] => Vec::new(),
            [prompt] if !prompt.echo && !answered => {
                answered = true;
                vec![password.to_owned()]
            }
            _ => {
                let asked: Vec<&str> = prompts.iter().map(|p| p.prompt.trim()).collect();
                return Err(SshError::Auth {
                    message: format!(
                        "{}:{} asks {} for more than a password: {}",
                        spec.host,
                        spec.port,
                        spec.user,
                        asked.join(" ")
                    ),
                });
            }
        };
        response = handle.authenticate_keyboard_interactive_respond(answers).await.map_err(network)?;
    }
}

async fn key(handle: &mut Handle<Client>, spec: &TunnelSpec, path: &Path) -> Result<(), SshError> {
    let key = russh::keys::load_secret_key(path, spec.secret()).map_err(|e| match e {
        russh::keys::Error::KeyIsEncrypted => SshError::MissingSecret,
        russh::keys::Error::SshKey(russh::keys::ssh_key::Error::Crypto) if spec.secret.is_some() => {
            SshError::Key { message: format!("the passphrase does not open {}", path.display()) }
        }
        e => SshError::Key { message: format!("{}: {e}", path.display()) },
    })?;
    let hash = rsa_hash(handle, key.public_key()).await?;
    let result = handle
        .authenticate_publickey(&spec.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await
        .map_err(network)?;
    accepted(result, spec, &format!("the key {}", path.display()))
}

/// The signature hash an RSA key signs with: SHA-2 when the server lists it, else `ssh-rsa`.
async fn rsa_hash(handle: &Handle<Client>, key: &PublicKey) -> Result<Option<HashAlg>, SshError> {
    if !key.algorithm().is_rsa() {
        return Ok(None);
    }
    Ok(handle.best_supported_rsa_hash().await.map_err(network)?.flatten())
}

async fn agent(handle: &mut Handle<Client>, spec: &TunnelSpec, socket: Option<&str>) -> Result<(), SshError> {
    let cannot_reach = |at: &dyn std::fmt::Display, e: russh::keys::Error| SshError::Agent {
        message: format!("cannot reach the SSH agent at {at}: {e}"),
    };
    #[cfg(unix)]
    {
        let path = match socket {
            Some(socket) => expand_home(socket),
            None => std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from).ok_or_else(|| SshError::Agent {
                message: "SSH_AUTH_SOCK is not set, so no SSH agent is reachable".into(),
            })?,
        };
        let client = AgentClient::connect_uds(&path).await.map_err(|e| cannot_reach(&path.display(), e))?;
        with_agent(handle, spec, client).await
    }
    #[cfg(windows)]
    {
        let pipe = socket.unwrap_or(r"\\.\pipe\openssh-ssh-agent");
        let client = AgentClient::connect_named_pipe(pipe).await.map_err(|e| cannot_reach(&pipe, e))?;
        with_agent(handle, spec, client).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (handle, spec, socket, cannot_reach);
        Err(SshError::Agent { message: "SSH agents are not supported on this platform".into() })
    }
}

async fn with_agent<S: AsyncRead + AsyncWrite + Unpin + Send>(
    handle: &mut Handle<Client>,
    spec: &TunnelSpec,
    mut agent: AgentClient<S>,
) -> Result<(), SshError> {
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| SshError::Agent { message: format!("the SSH agent did not list its keys: {e}") })?;
    let mut last = None;
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else { continue };
        let hash = rsa_hash(handle, &key).await?;
        let result = handle
            .authenticate_publickey_with(&spec.user, key, hash, &mut agent)
            .await
            .map_err(|e| SshError::Agent { message: e.to_string() })?;
        if result.success() {
            return Ok(());
        }
        last = Some(result);
    }
    match last {
        Some(result) => accepted(result, spec, "every key in the SSH agent"),
        None => Err(SshError::Agent { message: "the SSH agent holds no keys".into() }),
    }
}

pub(super) fn expand_home(path: &str) -> PathBuf {
    let home = || std::env::home_dir().unwrap_or_default();
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home().join(rest),
        None if path == "~" => home(),
        None => PathBuf::from(path),
    }
}
