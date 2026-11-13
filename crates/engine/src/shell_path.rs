use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(3);

/// What Windows can start a program from: the native installer's `claude.exe`, npm's `claude.cmd`.
/// npm also writes an extensionless shell script beside them, which Windows cannot start.
const WINDOWS_EXTENSIONS: &[&str] = &["exe", "cmd", "bat"];
const EXTENSIONS: &[&str] = if cfg!(windows) { WINDOWS_EXTENSIONS } else { &[] };

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
            for extra in [
                home.join(".local/bin"),
                home.join(".claude/local"),
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ] {
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

/// The home the CLIs install under and read `~` from: `HOME`, or `USERPROFILE` on Windows.
pub(crate) fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default()
}

/// The program `name` names in the first of `dirs` that has it.
pub(crate) fn find(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| executable(&dir.join(name)))
}

/// `path`, or on Windows `path` with the first extension that names a program there.
pub(crate) fn executable(path: &Path) -> Option<PathBuf> {
    executable_with(path, EXTENSIONS)
}

fn executable_with(path: &Path, extensions: &[&str]) -> Option<PathBuf> {
    let has_extension =
        path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.iter().any(|x| x.eq_ignore_ascii_case(e)));
    if extensions.is_empty() || has_extension {
        return is_executable(path).then(|| path.to_owned());
    }
    extensions
        .iter()
        .map(|ext| {
            let mut named = path.as_os_str().to_owned();
            named.push(".");
            named.push(ext);
            PathBuf::from(named)
        })
        .find(|p| is_executable(p))
}

/// Keeps a console program's window hidden: the release app has no console, so Windows would
/// open one for each child.
pub(crate) fn hide_console(command: &mut tokio::process::Command) {
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = command;
}

fn is_executable(path: &Path) -> bool {
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
    if cfg!(windows) {
        return None;
    }
    let shell = std::env::var_os("SHELL")?;
    let output = tokio::time::timeout(
        LOGIN_SHELL_TIMEOUT,
        tokio::process::Command::new(shell).args(["-lc", "printf %s \"$PATH\""]).kill_on_drop(true).output(),
    )
    .await
    .ok()?
    .ok()?;
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
        let path = std::env::join_paths([opt.clone(), ".".into(), "".into(), "node_modules/.bin".into(), usr.clone()])
            .unwrap();
        let child = search_path(&AgentPath::Fixed(path), &absolute("home/a")).await;
        assert_eq!(child, std::env::join_paths([&opt, &usr]).unwrap());
        assert_eq!(dirs(&child), [opt, usr]);
    }

    #[cfg(unix)]
    fn program(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, "").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    fn program(path: &Path) {
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn a_windows_lookup_takes_the_first_startable_extension_and_never_the_bare_shim() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude");
        program(&claude);
        assert_eq!(
            executable_with(&claude, WINDOWS_EXTENSIONS),
            None,
            "npm's extensionless shim is not a program on Windows"
        );
        program(&dir.path().join("claude.cmd"));
        assert_eq!(executable_with(&claude, WINDOWS_EXTENSIONS), Some(dir.path().join("claude.cmd")));
        program(&dir.path().join("claude.exe"));
        assert_eq!(
            executable_with(&claude, WINDOWS_EXTENSIONS),
            Some(dir.path().join("claude.exe")),
            "the native installer's exe wins over npm's cmd"
        );
        assert_eq!(
            executable_with(&dir.path().join("claude.cmd"), WINDOWS_EXTENSIONS),
            Some(dir.path().join("claude.cmd")),
            "a named extension is taken as given"
        );
        assert_eq!(executable_with(&claude, &[]), Some(claude.clone()), "elsewhere the name is the file");
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
