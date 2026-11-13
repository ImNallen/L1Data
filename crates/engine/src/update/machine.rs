use semver::Version;
use serde::Deserialize;
use serde_json::Value;

use super::Installed;
use crate::model::{
    ChangeItem, ReleaseChanges, RestartImpact, UpdateError, UpdateFailure, UpdateState, UpdateStatus, UpdateTrack,
    UpdatesOff,
};

const CHECK_EVERY_MS: i64 = 4 * 60 * 60 * 1000;
const RETRY_AFTER_MS: i64 = 60 * 60 * 1000;

pub(crate) fn feed(track: UpdateTrack) -> &'static str {
    match track {
        UpdateTrack::Stable => "https://github.com/ImNallen/L1Data/releases/latest/download/latest.json",
        UpdateTrack::Nightly => "https://github.com/ImNallen/L1Data/releases/download/nightly/latest.json",
    }
}

fn is_nightly(version: &str) -> bool {
    version.contains("-nightly.")
}

pub(crate) fn implied_track(version: &str) -> UpdateTrack {
    if is_nightly(version) { UpdateTrack::Nightly } else { UpdateTrack::Stable }
}

/// Only a nightly build on the Stable track takes an older version, so leaving Nightly installs
/// the latest stable. It is derived on each check, so a check that failed or was ignored after a
/// switch, or a restart in between, cannot strand the user on the old track. Semver precedence
/// orders 0.1.1-nightly.20261023.5 < 0.1.1-nightly.20261024.6 < 0.1.1.
pub(crate) fn wanted(installed: &Version, offered: &Version, track: UpdateTrack) -> Option<Direction> {
    let direction = if offered < installed { Direction::Downgrade } else { Direction::Upgrade };
    if track == UpdateTrack::Stable && is_nightly(&installed.to_string()) {
        (offered != installed).then_some(direction)
    } else {
        (offered > installed).then_some(direction)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Upgrade,
    Downgrade,
}

pub(crate) fn covers(accepted: &RestartImpact, now: &RestartImpact) -> bool {
    now.running.iter().all(|t| accepted.running.contains(t))
        && now.chats.iter().all(|c| accepted.chats.contains(c))
        && now
            .transactions
            .iter()
            .all(|t| accepted.transactions.iter().any(|a| a.tab_id == t.tab_id && a.has_writes >= t.has_writes))
}

pub(crate) fn percent(received: u64, total: Option<u64>) -> Option<u8> {
    total.filter(|&t| t > 0).map(|t| (received.min(t) * 100 / t) as u8)
}

#[derive(Deserialize)]
struct FeedRelease {
    version: String,
    items: Vec<FeedItem>,
}

#[derive(Deserialize)]
struct FeedItem {
    title: String,
    author: Option<String>,
    pr: Option<u32>,
}

/// The feed's `changes`. It is display only, so an entry this build cannot read is dropped and
/// never fails the check.
pub(crate) fn feed_changes(feed: &Value) -> Vec<(Version, ReleaseChanges)> {
    let Some(entries) = feed.get("changes").and_then(Value::as_array) else { return Vec::new() };
    entries
        .iter()
        .filter_map(|entry| {
            let release = FeedRelease::deserialize(entry).ok()?;
            let version = Version::parse(&release.version).ok()?;
            let items =
                release.items.into_iter().map(|i| ChangeItem { title: i.title, author: i.author, pr: i.pr }).collect();
            Some((version.clone(), ReleaseChanges { version: version.to_string(), items }))
        })
        .collect()
}

/// The releases a user moving from `installed` to `offered` gets, newest first. A downgrade gets none.
fn changes_between(
    installed: &Version,
    offered: &Version,
    mut changes: Vec<(Version, ReleaseChanges)>,
) -> Vec<ReleaseChanges> {
    changes.retain(|(v, _)| installed < v && v <= offered);
    changes.sort_by(|(a, _), (b, _)| b.cmp(a));
    changes.into_iter().map(|(_, c)| c).collect()
}

/// What a check found, before the policy judges it.
pub(crate) struct Found<R> {
    pub version: String,
    pub changes: Vec<(Version, ReleaseChanges)>,
    pub release: R,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ticket(u64);

pub(crate) struct Check {
    pub ticket: Ticket,
    pub feed: &'static str,
}

struct Offer<T> {
    version: Version,
    downgrade: bool,
    changes: Vec<ReleaseChanges>,
    held: T,
}

impl<T> Offer<T> {
    fn with<U>(&self, held: U) -> Offer<U> {
        Offer { version: self.version.clone(), downgrade: self.downgrade, changes: self.changes.clone(), held }
    }
}

enum Phase<R, V> {
    Disabled(UpdatesOff),
    Idle,
    /// The ticket drops a result that a track switch made stale.
    Checking(Ticket),
    UpToDate,
    Available(Offer<R>),
    Downloading(Offer<()>, Option<u8>),
    Downloaded(Offer<V>),
    Installing(Version),
    Failed(UpdateFailure),
}

pub(crate) struct Machine<R, V> {
    installed: Result<Version, UpdatesOff>,
    current_version: String,
    track: UpdateTrack,
    phase: Phase<R, V>,
    checked_at_ms: Option<i64>,
    failed_at_ms: Option<i64>,
    next_ticket: u64,
    rev: u64,
}

pub(crate) fn updatable(installed: &Installed) -> Result<Version, UpdatesOff> {
    installed.support.and_then(|()| Version::parse(&installed.version).map_err(|_| UpdatesOff::UnknownVersion))
}

impl<R, V> Machine<R, V> {
    pub(crate) fn new(installed: Installed, track: UpdateTrack) -> Self {
        let parsed = updatable(&installed);
        let phase = match &parsed {
            Ok(_) => Phase::Idle,
            Err(reason) => Phase::Disabled(*reason),
        };
        Self {
            installed: parsed,
            current_version: installed.version,
            track,
            phase,
            checked_at_ms: None,
            failed_at_ms: None,
            next_ticket: 0,
            rev: 0,
        }
    }

    pub(crate) fn track(&self) -> UpdateTrack {
        self.track
    }

    pub(crate) fn state(&self) -> UpdateState {
        UpdateState {
            rev: self.rev,
            track: self.track,
            current_version: self.current_version.clone(),
            status: self.status(),
            checked_at_ms: self.checked_at_ms,
        }
    }

    fn status(&self) -> UpdateStatus {
        match &self.phase {
            Phase::Disabled(reason) => UpdateStatus::Disabled { reason: *reason },
            Phase::Idle => UpdateStatus::Idle,
            Phase::Checking(_) => UpdateStatus::Checking,
            Phase::UpToDate => UpdateStatus::UpToDate,
            Phase::Available(o) => UpdateStatus::Available {
                version: o.version.to_string(),
                downgrade: o.downgrade,
                changes: o.changes.clone(),
            },
            Phase::Downloading(o, percent) => UpdateStatus::Downloading {
                version: o.version.to_string(),
                downgrade: o.downgrade,
                percent: *percent,
                changes: o.changes.clone(),
            },
            Phase::Downloaded(o) => UpdateStatus::Downloaded {
                version: o.version.to_string(),
                downgrade: o.downgrade,
                changes: o.changes.clone(),
            },
            Phase::Installing(version) => UpdateStatus::Installing { version: version.to_string() },
            Phase::Failed(failure) => UpdateStatus::Failed { failure: failure.clone() },
        }
    }

    /// A progress chunk inside the same whole percent publishes nothing.
    pub(crate) fn apply<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> (T, Option<UpdateState>) {
        let before = self.state();
        let out = f(self);
        if self.state() == before {
            return (out, None);
        }
        self.rev += 1;
        (out, Some(self.state()))
    }

    fn start_check(&mut self) -> Check {
        self.next_ticket += 1;
        let ticket = Ticket(self.next_ticket);
        self.phase = Phase::Checking(ticket);
        Check { ticket, feed: feed(self.track) }
    }

    /// A check in flight coalesces, and a held release is never thrown away by a background check.
    pub(crate) fn begin_check(&mut self) -> Option<Check> {
        matches!(self.phase, Phase::Idle | Phase::UpToDate | Phase::Failed(_)).then(|| self.start_check())
    }

    pub(crate) fn finish_check(
        &mut self,
        ticket: Ticket,
        fetched: Result<Option<Found<R>>, UpdateFailure>,
        now_ms: i64,
    ) {
        let (Phase::Checking(current), Ok(installed)) = (&self.phase, &self.installed) else { return };
        if *current != ticket {
            return;
        }
        let offered = fetched.and_then(|found| match found {
            None => Ok(None),
            Some(Found { version, changes, release }) => match Version::parse(&version) {
                Ok(version) => Ok(Some((version, changes, release))),
                Err(e) => Err(UpdateFailure::Other { message: format!("the feed offers version {version:?}: {e}") }),
            },
        });
        self.phase = match offered {
            Err(failure) => {
                self.failed_at_ms = Some(now_ms);
                Phase::Failed(failure)
            }
            Ok(found) => {
                self.checked_at_ms = Some(now_ms);
                let offer = found.and_then(|(version, changes, held)| {
                    let direction = wanted(installed, &version, self.track)?;
                    let changes = changes_between(installed, &version, changes);
                    Some(Offer { version, downgrade: direction == Direction::Downgrade, changes, held })
                });
                match offer {
                    Some(offer) => Phase::Available(offer),
                    None => Phase::UpToDate,
                }
            }
        };
    }

    fn holds(&self, version: &str) -> bool {
        match &self.phase {
            Phase::Downloading(o, _) => o.version.to_string() == version,
            Phase::Downloaded(o) => o.version.to_string() == version,
            Phase::Installing(v) => v.to_string() == version,
            _ => false,
        }
    }

    pub(crate) fn begin_download(&mut self, version: &str) -> Result<Option<R>, UpdateError> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Available(offer) if offer.version.to_string() == version => {
                self.phase = Phase::Downloading(offer.with(()), None);
                Ok(Some(offer.held))
            }
            other => {
                self.phase = other;
                if self.holds(version) { Ok(None) } else { Err(UpdateError::Stale) }
            }
        }
    }

    pub(crate) fn progress(&mut self, percent: Option<u8>) {
        if let Phase::Downloading(_, shown) = &mut self.phase {
            *shown = percent;
        }
    }

    pub(crate) fn finish_download(&mut self, downloaded: Result<V, UpdateFailure>, now_ms: i64) {
        let Phase::Downloading(offer, _) = &self.phase else { return };
        self.phase = match downloaded {
            Ok(verified) => Phase::Downloaded(offer.with(verified)),
            Err(failure) => {
                self.failed_at_ms = Some(now_ms);
                Phase::Failed(failure)
            }
        };
    }

    pub(crate) fn installing(&self, version: &str) -> Result<bool, UpdateError> {
        match &self.phase {
            Phase::Installing(_) if self.holds(version) => Ok(true),
            Phase::Downloaded(_) if self.holds(version) => Ok(false),
            _ => Err(UpdateError::Stale),
        }
    }

    pub(crate) fn begin_install(&mut self, version: &str) -> Result<Option<V>, UpdateError> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Downloaded(offer) if offer.version.to_string() == version => {
                self.phase = Phase::Installing(offer.version);
                Ok(Some(offer.held))
            }
            other => {
                self.phase = other;
                self.installing(version).map(|_| None)
            }
        }
    }

    pub(crate) fn install_failed(&mut self, failure: UpdateFailure, now_ms: i64) {
        if matches!(self.phase, Phase::Installing(_)) {
            self.failed_at_ms = Some(now_ms);
            self.phase = Phase::Failed(failure);
        }
    }

    pub(crate) fn set_track(&mut self, track: UpdateTrack) -> Result<Option<Check>, UpdateError> {
        if track == self.track {
            return Ok(None);
        }
        if matches!(self.phase, Phase::Downloading(..) | Phase::Installing(_)) {
            return Err(UpdateError::Busy);
        }
        self.track = track;
        Ok((!matches!(self.phase, Phase::Disabled(_))).then(|| self.start_check()))
    }

    pub(crate) fn due(&self, now_ms: i64) -> bool {
        let waited = |since: Option<i64>, wait: i64| since.is_none_or(|at| now_ms < at || now_ms - at >= wait);
        match self.phase {
            Phase::Idle => true,
            Phase::UpToDate => waited(self.checked_at_ms, CHECK_EVERY_MS),
            Phase::Failed(_) => waited(self.failed_at_ms, RETRY_AFTER_MS),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OpenTxn, TabId};

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn installed(version: &str) -> Installed {
        Installed { version: version.into(), support: Ok(()) }
    }

    fn offer<T>(held: T) -> Offer<T> {
        Offer { version: v("0.2.0"), downgrade: false, changes: vec![], held }
    }

    fn found(version: &str) -> Result<Option<Found<()>>, UpdateFailure> {
        Ok(Some(Found { version: version.into(), changes: vec![], release: () }))
    }

    fn at(phase: Phase<(), ()>) -> Machine<(), ()> {
        let mut m = Machine::new(installed("0.1.0"), UpdateTrack::Stable);
        m.phase = phase;
        m.next_ticket = 1;
        m
    }

    fn every() -> Vec<Machine<(), ()>> {
        vec![
            at(Phase::Disabled(UpdatesOff::DevBuild)),
            at(Phase::Idle),
            at(Phase::Checking(Ticket(1))),
            at(Phase::UpToDate),
            at(Phase::Available(offer(()))),
            at(Phase::Downloading(offer(()), Some(40))),
            at(Phase::Downloaded(offer(()))),
            at(Phase::Installing(v("0.2.0"))),
            at(Phase::Failed(UpdateFailure::Offline)),
        ]
    }

    fn table(event: impl Fn(&mut Machine<(), ()>)) -> Vec<(UpdateStatus, UpdateStatus)> {
        every()
            .into_iter()
            .map(|mut m| {
                let before = m.status();
                event(&mut m);
                (before, m.status())
            })
            .collect()
    }

    fn changed(rows: Vec<(UpdateStatus, UpdateStatus)>) -> Vec<(UpdateStatus, UpdateStatus)> {
        rows.into_iter().filter(|(from, to)| from != to).collect()
    }

    fn available(version: &str, downgrade: bool) -> UpdateStatus {
        UpdateStatus::Available { version: version.into(), downgrade, changes: vec![] }
    }
    fn downloading(percent: Option<u8>) -> UpdateStatus {
        UpdateStatus::Downloading { version: "0.2.0".into(), downgrade: false, percent, changes: vec![] }
    }
    fn downloaded() -> UpdateStatus {
        UpdateStatus::Downloaded { version: "0.2.0".into(), downgrade: false, changes: vec![] }
    }
    fn installing() -> UpdateStatus {
        UpdateStatus::Installing { version: "0.2.0".into() }
    }
    fn failed(failure: UpdateFailure) -> UpdateStatus {
        UpdateStatus::Failed { failure }
    }

    #[test]
    fn only_idle_up_to_date_and_failed_start_a_check() {
        for mut m in every() {
            let before = m.status();
            let starts = matches!(before, UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Failed { .. });
            let check = m.begin_check();
            assert_eq!(check.is_some(), starts, "{before:?}");
            assert_eq!(m.status(), if starts { UpdateStatus::Checking } else { before });
            if let Some(check) = check {
                assert_eq!(check.feed, feed(UpdateTrack::Stable));
            }
        }
    }

    #[test]
    fn only_the_check_in_flight_takes_its_result() {
        let offered = |m: &mut Machine<(), ()>| m.finish_check(Ticket(1), found("0.2.0"), 5);
        assert_eq!(changed(table(offered)), [(UpdateStatus::Checking, available("0.2.0", false))]);
        let nothing = |m: &mut Machine<(), ()>| m.finish_check(Ticket(1), Ok(None), 5);
        assert_eq!(changed(table(nothing)), [(UpdateStatus::Checking, UpdateStatus::UpToDate)]);
        let offline = |m: &mut Machine<(), ()>| m.finish_check(Ticket(1), Err(UpdateFailure::Offline), 5);
        assert_eq!(changed(table(offline)), [(UpdateStatus::Checking, failed(UpdateFailure::Offline))]);
    }

    #[test]
    fn a_result_from_before_a_track_switch_is_dropped() {
        let mut m = at(Phase::Idle);
        let stale = m.begin_check().unwrap();
        let fresh = m.set_track(UpdateTrack::Nightly).unwrap().unwrap();
        assert_eq!(fresh.feed, feed(UpdateTrack::Nightly));
        m.finish_check(stale.ticket, found("0.2.0"), 5);
        assert_eq!(m.status(), UpdateStatus::Checking);
        m.finish_check(fresh.ticket, found("0.2.1-nightly.20261024.6"), 6);
        assert_eq!(m.status(), available("0.2.1-nightly.20261024.6", false));
        assert_eq!(m.state().checked_at_ms, Some(6));
    }

    #[test]
    fn the_offer_policy() {
        use Direction::{Downgrade, Upgrade};
        let rows = [
            ("0.1.0", UpdateTrack::Stable, "0.2.0", Some(Upgrade)),
            ("0.1.0", UpdateTrack::Stable, "0.1.0", None),
            ("0.1.0", UpdateTrack::Stable, "0.0.9", None),
            ("0.1.1-nightly.20261023.5", UpdateTrack::Nightly, "0.1.1-nightly.20261024.6", Some(Upgrade)),
            ("0.1.1-nightly.20261023.5", UpdateTrack::Nightly, "0.1.1", Some(Upgrade)),
            ("0.1.1-nightly.20261023.5", UpdateTrack::Nightly, "0.1.1-nightly.20261022.4", None),
            ("0.1.1-nightly.20261023.5", UpdateTrack::Stable, "0.1.0", Some(Downgrade)),
            ("0.1.1-nightly.20261023.5", UpdateTrack::Stable, "0.1.1", Some(Upgrade)),
            ("0.1.0", UpdateTrack::Nightly, "0.1.1-nightly.20261023.7", Some(Upgrade)),
            ("0.1.1", UpdateTrack::Nightly, "0.1.1", None),
            ("0.1.1", UpdateTrack::Nightly, "0.1.1-nightly.20261023.7", None),
        ];
        for (running, track, offered, expected) in rows {
            assert_eq!(wanted(&v(running), &v(offered), track), expected, "{running} on {track:?} offered {offered}");
        }
    }

    fn entry(version: &str) -> Value {
        serde_json::json!({ "version": version, "items": [{ "title": format!("feat(app): ship {version}"), "author": "ImNallen", "pr": 50 }] })
    }

    fn versions(status: &UpdateStatus) -> Vec<&str> {
        let changes = match status {
            UpdateStatus::Available { changes, .. }
            | UpdateStatus::Downloading { changes, .. }
            | UpdateStatus::Downloaded { changes, .. } => changes,
            other => panic!("no changes in {other:?}"),
        };
        changes.iter().map(|c| c.version.as_str()).collect()
    }

    fn check(running: &str, track: UpdateTrack, offered: &str, feed: Value) -> Machine<(), ()> {
        let mut m = Machine::new(installed(running), track);
        let check = m.begin_check().unwrap();
        m.finish_check(
            check.ticket,
            Ok(Some(Found { version: offered.into(), changes: feed_changes(&feed), release: () })),
            5,
        );
        m
    }

    #[test]
    fn an_offer_lists_each_release_after_the_running_build_newest_first_until_it_installs() {
        let feed = serde_json::json!({ "version": "0.2.0", "changes": [entry("0.1.1"), entry("0.3.0"), entry("0.2.0"), entry("0.1.0"), entry("0.0.9")] });
        let mut m = check("0.1.0", UpdateTrack::Stable, "0.2.0", feed);
        assert_eq!(versions(&m.status()), ["0.2.0", "0.1.1"]);
        let UpdateStatus::Available { changes, .. } = m.status() else { panic!() };
        assert_eq!(
            changes[0].items,
            [ChangeItem { title: "feat(app): ship 0.2.0".into(), author: Some("ImNallen".into()), pr: Some(50) }]
        );
        m.begin_download("0.2.0").unwrap();
        m.progress(Some(40));
        assert_eq!(versions(&m.status()), ["0.2.0", "0.1.1"]);
        m.finish_download(Ok(()), 6);
        assert_eq!(versions(&m.status()), ["0.2.0", "0.1.1"]);
    }

    #[test]
    fn an_older_stable_offered_to_a_nightly_lists_no_changes() {
        let feed = serde_json::json!({ "version": "0.1.0", "changes": [entry("0.1.0"), entry("0.0.9")] });
        let m = check("0.1.1-nightly.20261023.5", UpdateTrack::Stable, "0.1.0", feed);
        assert_eq!(m.status(), UpdateStatus::Available { version: "0.1.0".into(), downgrade: true, changes: vec![] });
    }

    #[test]
    fn a_change_list_this_build_cannot_read_drops_only_what_it_cannot_read() {
        assert_eq!(feed_changes(&serde_json::json!({ "version": "0.2.0" })), []);
        assert_eq!(feed_changes(&serde_json::json!({ "version": "0.2.0", "changes": { "0.2.0": [] } })), []);
        let feed = serde_json::json!({ "version": "0.2.0", "changes": [
            { "version": "0.2.0", "items": [{ "title": "fix(ui): keep the caret", "author": null, "pr": null }, { "title": "docs: say why" }] },
            { "version": "next", "items": [] },
            { "version": "0.1.9", "items": [{ "title": "feat(app): x", "pr": "50" }] },
            { "version": "0.1.8" },
            "0.1.7",
            { "version": "0.1.6", "items": [], "date": "2026-10-20" },
        ] });
        let read = feed_changes(&feed);
        let shown: Vec<_> = read.iter().map(|(_, c)| c.clone()).collect();
        let item = |title: &str| ChangeItem { title: title.into(), author: None, pr: None };
        assert_eq!(
            shown,
            [
                ReleaseChanges {
                    version: "0.2.0".into(),
                    items: vec![item("fix(ui): keep the caret"), item("docs: say why")]
                },
                ReleaseChanges { version: "0.1.6".into(), items: vec![] },
            ]
        );
        let m = check("0.1.0", UpdateTrack::Stable, "0.2.0", feed);
        assert_eq!(versions(&m.status()), ["0.2.0", "0.1.6"], "an unreadable list never blocks the offer");
    }

    #[test]
    fn a_build_follows_the_track_its_version_implies() {
        assert_eq!(implied_track("0.1.1-nightly.20261023.5"), UpdateTrack::Nightly);
        assert_eq!(implied_track("0.1.0"), UpdateTrack::Stable);
        assert_eq!(implied_track("0.2.0-rc.1"), UpdateTrack::Stable);
    }

    #[test]
    fn an_unparsable_offer_fails_and_is_not_offered() {
        let mut m = at(Phase::Checking(Ticket(1)));
        m.finish_check(Ticket(1), found("latest"), 5);
        assert!(
            matches!(m.status(), UpdateStatus::Failed { failure: UpdateFailure::Other { .. } }),
            "{:?}",
            m.status()
        );
        assert_eq!(m.state().checked_at_ms, None);
    }

    #[test]
    fn an_unparsable_or_unsupported_build_is_disabled() {
        let m = Machine::<(), ()>::new(installed("dev"), UpdateTrack::Stable);
        assert_eq!(m.status(), UpdateStatus::Disabled { reason: UpdatesOff::UnknownVersion });
        let m = Machine::<(), ()>::new(
            Installed { version: "0.1.0".into(), support: Err(UpdatesOff::NotAppImage) },
            UpdateTrack::Stable,
        );
        assert_eq!(m.status(), UpdateStatus::Disabled { reason: UpdatesOff::NotAppImage });
    }

    #[test]
    fn download_takes_the_named_release_once_and_a_repeat_is_a_no_op() {
        let mut m = at(Phase::Available(offer(())));
        assert!(matches!(m.begin_download("0.1.9"), Err(UpdateError::Stale)));
        assert_eq!(m.status(), available("0.2.0", false));
        assert_eq!(m.begin_download("0.2.0").unwrap(), Some(()));
        assert_eq!(m.status(), downloading(None));
        assert_eq!(m.begin_download("0.2.0").unwrap(), None);
        for mut m in every() {
            let before = m.status();
            let result = m.begin_download("0.2.0");
            match &before {
                UpdateStatus::Available { .. } => assert_eq!(m.status(), downloading(None)),
                UpdateStatus::Downloading { .. }
                | UpdateStatus::Downloaded { .. }
                | UpdateStatus::Installing { .. } => {
                    assert_eq!(result.unwrap(), None, "{before:?}");
                    assert_eq!(m.status(), before);
                }
                _ => {
                    assert!(matches!(result, Err(UpdateError::Stale)), "{before:?}");
                    assert_eq!(m.status(), before);
                }
            }
        }
    }

    #[test]
    fn only_a_download_in_progress_reports_progress_or_finishes() {
        assert_eq!(changed(table(|m| m.progress(Some(75)))), [(downloading(Some(40)), downloading(Some(75)))]);
        assert_eq!(changed(table(|m| m.finish_download(Ok(()), 5))), [(downloading(Some(40)), downloaded())]);
        assert_eq!(
            changed(table(|m| m.finish_download(Err(UpdateFailure::BadSignature), 5))),
            [(downloading(Some(40)), failed(UpdateFailure::BadSignature))]
        );
    }

    #[test]
    fn progress_publishes_only_a_new_whole_percent() {
        let mut m = at(Phase::Downloading(offer(()), Some(40)));
        let (_, published) = m.apply(|m| m.progress(percent(401, Some(1000))));
        assert_eq!(published, None);
        let (_, published) = m.apply(|m| m.progress(percent(410, Some(1000))));
        assert_eq!(published.map(|s| (s.rev, s.status)), Some((1, downloading(Some(41)))));
        assert_eq!(percent(0, Some(200)), Some(0));
        assert_eq!(percent(199, Some(200)), Some(99));
        assert_eq!(percent(250, Some(200)), Some(100));
        assert_eq!(percent(10, None), None);
        assert_eq!(percent(10, Some(0)), None);
    }

    #[test]
    fn install_takes_the_verified_bytes_exactly_once() {
        let mut m = at(Phase::Downloaded(offer(())));
        assert!(matches!(m.begin_install("0.1.9"), Err(UpdateError::Stale)));
        assert!(!m.installing("0.2.0").unwrap());
        assert_eq!(m.begin_install("0.2.0").unwrap(), Some(()));
        assert_eq!(m.status(), installing());
        assert!(m.installing("0.2.0").unwrap());
        assert_eq!(m.begin_install("0.2.0").unwrap(), None);
        for mut m in every() {
            let before = m.status();
            if !matches!(before, UpdateStatus::Downloaded { .. } | UpdateStatus::Installing { .. }) {
                assert!(matches!(m.begin_install("0.2.0"), Err(UpdateError::Stale)), "{before:?}");
                assert_eq!(m.status(), before);
            }
        }
        let failure = UpdateFailure::ReadOnlyLocation;
        assert_eq!(
            changed(table(|m| m.install_failed(failure.clone(), 5))),
            [(installing(), failed(UpdateFailure::ReadOnlyLocation))]
        );
    }

    #[test]
    fn a_switch_is_busy_only_while_downloading_or_installing() {
        for mut m in every() {
            let before = m.status();
            let switched = m.set_track(UpdateTrack::Nightly);
            match &before {
                UpdateStatus::Downloading { .. } | UpdateStatus::Installing { .. } => {
                    assert!(matches!(switched, Err(UpdateError::Busy)), "{before:?}");
                    assert_eq!((m.status(), m.track()), (before, UpdateTrack::Stable));
                }
                UpdateStatus::Disabled { .. } => {
                    assert!(switched.unwrap().is_none());
                    assert_eq!((m.status(), m.track()), (before, UpdateTrack::Nightly));
                }
                _ => {
                    assert_eq!(switched.unwrap().map(|c| c.feed), Some(feed(UpdateTrack::Nightly)), "{before:?}");
                    assert_eq!((m.status(), m.track()), (UpdateStatus::Checking, UpdateTrack::Nightly));
                }
            }
        }
    }

    #[test]
    fn the_same_track_again_changes_nothing() {
        for mut m in every() {
            let before = m.state();
            let (switched, published) = m.apply(|m| m.set_track(UpdateTrack::Stable));
            assert!(switched.unwrap().is_none());
            assert_eq!((published, m.state()), (None, before));
        }
    }

    #[test]
    fn checks_are_due_every_four_hours_or_one_after_a_failure() {
        const HOUR: i64 = 60 * 60 * 1000;
        let mut m = at(Phase::Idle);
        assert!(m.due(0));
        m.phase = Phase::UpToDate;
        m.checked_at_ms = Some(10 * HOUR);
        assert!(!m.due(13 * HOUR));
        assert!(m.due(14 * HOUR));
        assert!(m.due(9 * HOUR), "a clock that went backwards");
        m.phase = Phase::Failed(UpdateFailure::Offline);
        m.failed_at_ms = Some(20 * HOUR);
        assert!(!m.due(20 * HOUR + HOUR - 1));
        assert!(m.due(21 * HOUR));
        let never = every().into_iter().filter(|m| {
            !matches!(m.status(), UpdateStatus::Idle | UpdateStatus::UpToDate | UpdateStatus::Failed { .. })
        });
        for m in never {
            assert!(!m.due(i64::MAX), "{:?}", m.status());
        }
    }

    #[test]
    fn consent_covers_every_interruption_and_no_new_writes() {
        let (a, b) = (TabId(uuid::Uuid::from_u128(1)), TabId(uuid::Uuid::from_u128(2)));
        let txn = |tab_id, has_writes| OpenTxn { tab_id, has_writes };
        let accepted = RestartImpact { running: vec![a], transactions: vec![txn(b, false)], chats: vec![] };
        assert!(covers(&accepted, &accepted));
        assert!(covers(&accepted, &RestartImpact::default()));
        assert!(!covers(&accepted, &RestartImpact { running: vec![b], ..accepted.clone() }));
        assert!(!covers(&accepted, &RestartImpact { transactions: vec![txn(b, true)], ..accepted.clone() }));
        assert!(!covers(
            &accepted,
            &RestartImpact { chats: vec![crate::model::ChatId(uuid::Uuid::from_u128(3))], ..accepted.clone() }
        ));
        let with_writes = RestartImpact { transactions: vec![txn(b, true)], ..accepted.clone() };
        assert!(covers(&with_writes, &accepted));
    }
}
