use parking_lot::{Mutex, MutexGuard};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use uuid::Uuid;

use crate::model::AssistantAccess;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccessLease {
    pub mode: AssistantAccess,
    pub generation: Uuid,
}

impl AccessLease {
    pub(crate) fn new(mode: AssistantAccess) -> Self {
        Self { mode, generation: Uuid::new_v4() }
    }
}

pub(crate) struct Access {
    state: Mutex<AccessLease>,
    changed: watch::Sender<AccessLease>,
}

impl Access {
    pub(crate) fn new(lease: AccessLease) -> Self {
        Self { state: Mutex::new(lease), changed: watch::channel(lease).0 }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, AccessLease> {
        self.state.lock()
    }

    pub(crate) fn changed(&self, lease: AccessLease) {
        self.changed.send_replace(lease);
    }

    pub(crate) fn with<T>(&self, lease: AccessLease, deliver: impl FnOnce() -> T) -> Option<T> {
        let current = self.lock();
        (*current == lease).then(deliver)
    }

    pub(crate) async fn revoked(&self, lease: AccessLease) {
        let mut changed = self.changed.subscribe();
        let _ = changed.wait_for(|current| *current != lease).await;
    }
}
