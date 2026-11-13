use std::time::Duration;

use futures_util::future::join_all;

use super::agent::StartError;
use super::{HostEnv, claude};
use crate::Shared;
use crate::console::now_ms;
use crate::model::{LimitLevel, LimitsOutcome, LimitsReport, ProviderKind, SourceLimits, default_config};

const LIMITS_TIMEOUT: Duration = Duration::from_secs(8);
const LOW_AT_LEFT: u8 = 20;

impl Shared {
    pub(crate) async fn read_limits(&self) -> LimitsReport {
        let host = self.host_env().await;
        let sources = join_all(ProviderKind::ALL.map(|kind| self.read_source(kind, host))).await;
        LimitsReport { checked_at_ms: now_ms(), sources }
    }

    async fn read_source(&self, kind: ProviderKind, host: &HostEnv) -> SourceLimits {
        let cfg = self.provider_config(kind).unwrap_or_else(|| default_config(kind));
        let read = async {
            match kind {
                ProviderKind::Claude => claude::read_limits(self, &cfg, host).await,
            }
        };
        let outcome = tokio::time::timeout(LIMITS_TIMEOUT, read).await.unwrap_or_else(|_| LimitsOutcome::Failed {
            message: format!("no answer within {} s", LIMITS_TIMEOUT.as_secs()),
        });
        SourceLimits { source: kind, name: cfg.display_name, outcome }
    }
}

pub(crate) fn start_failed(e: StartError) -> LimitsOutcome {
    match e {
        StartError::NotFound { binary } => LimitsOutcome::NotFound { binary },
        other => LimitsOutcome::Failed { message: other.to_string() },
    }
}

pub(crate) fn level(left_percent: u8, severity: Option<&str>) -> LimitLevel {
    match (left_percent, severity) {
        (0, _) => LimitLevel::Exhausted,
        (left, _) if left <= LOW_AT_LEFT => LimitLevel::Low,
        (_, Some(severity)) if severity != "normal" => LimitLevel::Low,
        _ => LimitLevel::Normal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low_is_twenty_left_or_any_severity_but_normal_and_zero_is_exhausted() {
        assert_eq!(level(21, Some("normal")), LimitLevel::Normal);
        assert_eq!(level(20, None), LimitLevel::Low);
        assert_eq!(level(50, Some("warning")), LimitLevel::Low);
        assert_eq!(level(0, Some("normal")), LimitLevel::Exhausted);
        assert_eq!(level(100, None), LimitLevel::Normal);
    }
}
