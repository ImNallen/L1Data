use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// The PATH the engine looks up a child program on (an agent CLI, or `az`), and the PATH that
/// program runs with. Only its absolute entries are kept, so neither the lookup nor a child's
/// `#!/usr/bin/env` line can pick a program from the working directory. `LoginShell` is the app's:
/// a Finder-launched app gets launchd's PATH, which misses where `claude`, `node`, and `az` live,
/// so the login shell's PATH comes first, then the inherited one, then the usual install
/// directories. `Fixed` is the given PATH, with no shell and nothing added; tests pass an empty
/// one, so nothing installed on the machine runs.
#[derive(Debug, Clone)]
pub enum AgentPath {
    LoginShell,
    Fixed(OsString),
}

pub(crate) async fn search_path(agent_path: &AgentPath, home: &Path) -> OsString {
    let dirs = match agent_path {
        AgentPath::Fixed(path) => dirs(path),
        AgentPath::LoginShell => {
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            let shell_path = login_shell_path().await.unwrap_or_default();
            let mut dirs: Vec<PathBuf> = dirs(&shell_path).into_iter().chain(dirs(&inherited)).collect();
            for extra in [home.join(".local/bin"), home.join(".claude/local"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")] {
                if extra.is_absolute() && !dirs.contains(&extra) {
                    dirs.push(extra);
                }
            }
            dirs
        }
    };
    std::env::join_paths(dirs).unwrap_or_default()
}

pub(crate) fn dirs(path: &OsString) -> Vec<PathBuf> {
    std::env::split_paths(path).filter(|p| p.is_absolute()).collect()
}

pub(crate) fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

async fn login_shell_path() -> Option<OsString> {
    let shell = std::env::var_os("SHELL")?;
    let output = tokio::time::timeout(LOGIN_SHELL_TIMEOUT, tokio::process::Command::new(shell).args(["-lc", "printf %s \"$PATH\""]).kill_on_drop(true).output()).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let path = text.lines().last()?.trim();
    (!path.is_empty()).then(|| OsString::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute(path: &str) -> PathBuf {
        Path::new(if cfg!(windows) { "C:\\" } else { "/" }).join(path)
    }

    #[tokio::test]
    async fn a_child_gets_and_searches_only_absolute_path_entries() {
        let (opt, usr) = (absolute("opt/bin"), absolute("usr/bin"));
        let path = std::env::join_paths([opt.clone(), ".".into(), "".into(), "node_modules/.bin".into(), usr.clone()]).unwrap();
        let child = search_path(&AgentPath::Fixed(path), &absolute("home/a")).await;
        assert_eq!(child, std::env::join_paths([&opt, &usr]).unwrap());
        assert_eq!(dirs(&child), [opt, usr]);
    }

    #[tokio::test]
    async fn an_unset_or_relative_home_adds_no_relative_install_directory() {
        for home in ["", "relative/home"] {
            let path = search_path(&AgentPath::LoginShell, Path::new(home)).await;
            let entries: Vec<PathBuf> = std::env::split_paths(&path).collect();
            assert!(entries.iter().all(|p| p.is_absolute()), "HOME {home:?}: {entries:?}");
            if cfg!(unix) {
                assert!(entries.contains(&PathBuf::from("/usr/local/bin")), "HOME {home:?}: {entries:?}");
            }
        }
        let home = absolute("home/a");
        let path = search_path(&AgentPath::LoginShell, &home).await;
        assert!(std::env::split_paths(&path).any(|p| p == home.join(".local/bin")));
    }
}
