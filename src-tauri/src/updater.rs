use std::io::ErrorKind;

use engine::model::{UpdateFailure, UpdatesOff};
use engine::{BoxFuture, Installed, Progress, Release, UpdateTransport, Verified};
use tauri::plugin::TauriPlugin;
use tauri::{AppHandle, Runtime, Url};
use tauri_plugin_updater::{Config, Error, Update, UpdaterExt};

pub fn plugin<R: Runtime>() -> TauriPlugin<R, Config> {
    tauri_plugin_updater::Builder::new().build()
}

pub struct TauriTransport {
    app: AppHandle,
}

impl TauriTransport {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }

    #[cfg(target_os = "linux")]
    fn replaceable(&self) -> bool {
        use tauri::Manager;
        self.app.env().appimage.is_some()
    }

    #[cfg(not(target_os = "linux"))]
    fn replaceable(&self) -> bool {
        true
    }
}

impl UpdateTransport for TauriTransport {
    fn installed(&self) -> Installed {
        let support = if cfg!(debug_assertions) {
            Err(UpdatesOff::DevBuild)
        } else if tauri_plugin_updater::target().is_none() {
            Err(UpdatesOff::UnsupportedTarget)
        } else if !self.replaceable() {
            Err(UpdatesOff::NotAppImage)
        } else {
            Ok(())
        };
        Installed { version: self.app.package_info().version.to_string(), support }
    }

    fn fetch<'a>(&'a self, feed: &'a str) -> BoxFuture<'a, Result<Option<Box<dyn Release>>, UpdateFailure>> {
        Box::pin(async move {
            let url: Url = feed.parse().map_err(|e| UpdateFailure::Other { message: format!("{feed}: {e}") })?;
            // The engine decides which version counts as an update, so this accepts every one. That is
            // safe only because `requireSignedVersion` binds the announced version to the signed
            // artifact, so a crafted feed cannot pair a new version number with an old signed build.
            let updater = self
                .app
                .updater_builder()
                .endpoints(vec![url])
                .map_err(classify)?
                .version_comparator(|_, _| true)
                .build()
                .map_err(classify)?;
            match updater.check().await {
                Ok(found) => Ok(found.map(|update| Box::new(TauriRelease(update)) as Box<dyn Release>)),
                Err(Error::ReleaseNotFound | Error::TargetNotFound(_) | Error::TargetsNotFound(_)) => Ok(None),
                Err(error) => Err(classify(error)),
            }
        })
    }

    fn relaunch(&self) {
        self.app.request_restart();
    }
}

struct TauriRelease(Update);

impl Release for TauriRelease {
    fn version(&self) -> &str {
        &self.0.version
    }

    fn feed(&self) -> &serde_json::Value {
        &self.0.raw_json
    }

    fn download(
        self: Box<Self>,
        mut progress: Progress,
    ) -> BoxFuture<'static, Result<Box<dyn Verified>, UpdateFailure>> {
        Box::pin(async move {
            let mut received = 0;
            let on_chunk = |chunk: usize, total| {
                received += chunk as u64;
                progress(received, total);
            };
            let bytes = self.0.download(on_chunk, || {}).await.map_err(classify)?;
            Ok(Box::new(TauriVerified { update: self.0, bytes }) as Box<dyn Verified>)
        })
    }
}

struct TauriVerified {
    update: Update,
    bytes: Vec<u8>,
}

impl Verified for TauriVerified {
    fn install(self: Box<Self>) -> Result<(), UpdateFailure> {
        self.update.install(&self.bytes).map_err(classify)
    }
}

fn classify(error: Error) -> UpdateFailure {
    eprintln!("updater: {error}");
    match error {
        Error::Reqwest(_) | Error::Network(_) => UpdateFailure::Offline,
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => UpdateFailure::BadSignature,
        Error::Io(e) if matches!(e.kind(), ErrorKind::ReadOnlyFilesystem | ErrorKind::CrossesDevices) => {
            UpdateFailure::ReadOnlyLocation
        }
        Error::TempDirNotOnSameMountPoint => UpdateFailure::ReadOnlyLocation,
        Error::Io(e) if e.kind() == ErrorKind::PermissionDenied => UpdateFailure::PermissionDenied,
        other => UpdateFailure::Other { message: other.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_updater_rs_names_the_plugin_and_it_requires_signed_versions() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            let name = path.file_name().unwrap().to_string_lossy();
            assert_eq!(
                text.contains("tauri_plugin_updater"),
                name == "updater.rs",
                "{name}: only updater.rs may name the updater plugin"
            );
        }
        let config: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tauri.conf.json")).unwrap(),
        )
        .unwrap();
        let updater = &config["plugins"]["updater"];
        assert_eq!(
            updater["requireSignedVersion"], true,
            "the accept-all comparator is safe only when the signature binds the version"
        );
        assert!(updater["pubkey"].as_str().is_some_and(|key| !key.is_empty()));
    }

    #[test]
    fn plugin_errors_become_update_failures() {
        let rows = [
            (Error::Network("connection reset".into()), UpdateFailure::Offline),
            (Error::SignatureUtf8("not base64".into()), UpdateFailure::BadSignature),
            (Error::MissingSignedVersion, UpdateFailure::BadSignature),
            (
                Error::SignedVersionMismatch { signed: "0.1.0".into(), announced: "0.9.0".into() },
                UpdateFailure::BadSignature,
            ),
            (Error::Io(ErrorKind::ReadOnlyFilesystem.into()), UpdateFailure::ReadOnlyLocation),
            (Error::Io(ErrorKind::CrossesDevices.into()), UpdateFailure::ReadOnlyLocation),
            (Error::TempDirNotOnSameMountPoint, UpdateFailure::ReadOnlyLocation),
            (Error::Io(ErrorKind::PermissionDenied.into()), UpdateFailure::PermissionDenied),
            (
                Error::BinaryNotFoundInArchive,
                UpdateFailure::Other { message: "binary for the current target not found in the archive".into() },
            ),
        ];
        for (error, failure) in rows {
            let shown = error.to_string();
            assert_eq!(classify(error), failure, "{shown}");
        }
    }
}
