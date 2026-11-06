use std::sync::{Arc, RwLock};
use std::time::Instant;

use crate::aggregate::ColumnText;
use crate::model::{
    AllRows, ColSpan, ColumnMeta, ConnectionId, DatabaseKind, DatabaseRef, PlanFormat, QueryError, ResultCell, ResultError, ResultFind, ResultFindDirection,
    ResultFindTarget, ResultMeta, ResultRef, RowPage, RowSpan, RunId, RunOutput, TabId, TruncateReason,
};

pub const CELL_CHAR_LIMIT: usize = 2_000;
pub const PAGE_LEN_MAX: u32 = 2_000;

const NULL: u32 = u32::MAX;

pub struct ResultSet {
    pub database_kind: DatabaseKind,
    pub r#ref: ResultRef,
    pub db: DatabaseRef,
    pub columns: Vec<ColumnMeta>,
    /// What produced these rows. Set when the result is created and never changed, and dropped
    /// with the result at the next run or tab close, so no other structure keeps SQL per result.
    pub origin: Origin,
    pub plan_format: Option<PlanFormat>,
    data: RwLock<Arena>,
}

pub struct Origin {
    pub rerun: Rerun,
    /// This result's position among the result sets its SQL returns. A SQL Server batch can
    /// return several; a PostgreSQL statement returns one.
    pub nth: u16,
    pub schema: Option<String>,
    pub page: bool,
}

#[derive(Clone)]
pub enum Rerun {
    Ready(Arc<str>),
    Unvetted { sql: Arc<str>, calls: Arc<[String]> },
    Refused(&'static str),
}

impl Rerun {
    pub fn sql(&self) -> Option<&Arc<str>> {
        match self {
            Rerun::Ready(sql) | Rerun::Unvetted { sql, .. } => Some(sql),
            Rerun::Refused(_) => None,
        }
    }
}

impl Origin {
    pub fn all_rows(&self) -> AllRows {
        match &self.rerun {
            Rerun::Ready(_) if self.page => AllRows::Table,
            Rerun::Ready(_) => AllRows::Statement,
            Rerun::Unvetted { calls, .. } => AllRows::Confirm { calls: calls.to_vec() },
            Rerun::Refused(reason) => AllRows::Refused { reason: (*reason).to_owned() },
        }
    }
}

struct Arena {
    text: String,
    cells: Vec<(u32, u32)>,
    ncols: usize,
    rows_seen: u64,
    truncated: Option<TruncateReason>,
    elapsed_ms: u32,
}

pub struct Caps {
    pub max_rows: u32,
    pub max_bytes: u64,
}

impl ResultSet {
    pub fn new(r#ref: ResultRef, db: DatabaseRef, database_kind: DatabaseKind, columns: Vec<ColumnMeta>, origin: Origin) -> Self {
        let ncols = columns.len();
        Self {
            database_kind,
            r#ref,
            db,
            columns,
            origin,
            plan_format: None,
            data: RwLock::new(Arena {
                text: String::new(),
                cells: Vec::new(),
                ncols,
                rows_seen: 0,
                truncated: None,
                elapsed_ms: 0,
            }),
        }
    }

    pub fn push_row<'a>(&self, cells: impl Iterator<Item = Option<&'a str>>, caps: &Caps) {
        let mut a = self.data.write().unwrap();
        a.rows_seen += 1;
        if a.truncated.is_some() {
            return;
        }
        if a.row_count() >= caps.max_rows {
            a.truncated = Some(TruncateReason::RowLimit);
            return;
        }
        let start_text = a.text.len();
        let start_cells = a.cells.len();
        for cell in cells {
            match cell {
                None => a.cells.push((0, NULL)),
                Some(s) => {
                    let start = a.text.len() as u32;
                    a.text.push_str(s);
                    a.cells.push((start, s.len() as u32));
                }
            }
        }
        if a.text.len() as u64 > caps.max_bytes || a.text.len() >= NULL as usize {
            a.text.truncate(start_text);
            a.cells.truncate(start_cells);
            a.truncated = Some(TruncateReason::ByteLimit);
        }
    }

    pub fn set_elapsed(&self, elapsed_ms: u32) {
        self.data.write().unwrap().elapsed_ms = elapsed_ms;
    }

    pub fn meta(&self) -> ResultMeta {
        let a = self.data.read().unwrap();
        ResultMeta {
            database_kind: self.database_kind,
            r#ref: self.r#ref,
            db: self.db.clone(),
            columns: self.columns.clone(),
            row_count: a.row_count(),
            rows_seen: a.rows_seen,
            truncated: a.truncated,
            elapsed_ms: a.elapsed_ms,
            all_rows: self.origin.all_rows(),
        }
    }

    pub fn page(&self, start: u32, len: u32) -> Option<RowPage> {
        let a = self.data.read().unwrap();
        let count = a.row_count();
        if start > count {
            return None;
        }
        let end = start.saturating_add(len.min(PAGE_LEN_MAX)).min(count);
        let mut cut = Vec::new();
        let rows = (start..end)
            .map(|r| {
                (0..a.ncols)
                    .map(|c| {
                        let value = a.cell(r as usize, c)?;
                        if value.chars().count() > CELL_CHAR_LIMIT {
                            cut.push((r, c as u16));
                            let keep = value.char_indices().nth(CELL_CHAR_LIMIT).map_or(value.len(), |(b, _)| b);
                            Some(value[..keep].to_owned())
                        } else {
                            Some(value.to_owned())
                        }
                    })
                    .collect()
            })
            .collect();
        Some(RowPage { start, rows, cut })
    }

    pub fn cell(&self, row: u32, col: u16) -> Option<Option<String>> {
        let a = self.data.read().unwrap();
        if row >= a.row_count() || col as usize >= a.ncols {
            return None;
        }
        Some(a.cell(row as usize, col as usize).map(str::to_owned))
    }

    pub(crate) fn find(
        &self,
        query: &str,
        direction: ResultFindDirection,
        current: impl Fn() -> bool,
    ) -> Result<ResultFind, ResultError> {
        let row_count = self.data.read().unwrap().row_count();
        let ncols = self.columns.len();
        let anchor = match direction {
            ResultFindDirection::First => None,
            ResultFindDirection::Next { anchor } | ResultFindDirection::Previous { anchor } => {
                Some(anchor)
            }
        };
        if anchor.is_some_and(|c| c.row >= row_count || c.col as usize >= ncols) {
            return Err(ResultError::OutOfRange);
        }
        let mut found = ResultFind {
            count: 0,
            target: None,
            row_count,
        };
        if query.is_empty() {
            return Ok(found);
        }
        let (needle, prefix) = find_pattern(query);
        let mut first = None;
        let mut last = None;
        let mut relative = None;
        for row in 0..row_count {
            for col in 0..ncols {
                let hit = self.matches_cell(row, col, &needle, &prefix, &current)?;
                if !hit {
                    continue;
                }
                found.count += 1;
                let cell = ResultCell {
                    row,
                    col: col as u16,
                };
                let target = || ResultFindTarget {
                    cell,
                    ordinal: found.count,
                };
                if first.is_none() {
                    first = Some(target());
                }
                last = Some(target());
                if let Some(anchor) = anchor {
                    let before = (cell.row, cell.col) < (anchor.row, anchor.col);
                    let after = (cell.row, cell.col) > (anchor.row, anchor.col);
                    match direction {
                        ResultFindDirection::Next { .. } if after && relative.is_none() => {
                            relative = Some(target())
                        }
                        ResultFindDirection::Previous { .. } if before => relative = Some(target()),
                        _ => (),
                    }
                }
            }
        }
        found.target = relative.or(match direction {
            ResultFindDirection::Previous { .. } => last,
            _ => first,
        });
        if !current() {
            return Err(ResultError::Gone);
        }
        Ok(found)
    }

    pub(crate) fn cell_matches(
        &self,
        cell: ResultCell,
        query: &str,
        current: impl Fn() -> bool,
    ) -> Result<bool, ResultError> {
        if cell.row >= self.data.read().unwrap().row_count()
            || cell.col as usize >= self.columns.len()
        {
            return Err(ResultError::OutOfRange);
        }
        if query.is_empty() {
            return Ok(false);
        }
        let (needle, prefix) = find_pattern(query);
        self.matches_cell(cell.row, cell.col as usize, &needle, &prefix, &current)
    }

    fn matches_cell(
        &self,
        row: u32,
        col: usize,
        needle: &[char],
        prefix: &[usize],
        current: &impl Fn() -> bool,
    ) -> Result<bool, ResultError> {
        let mut offset = 0;
        let mut matched = 0;
        let hit = loop {
            if !current() {
                return Err(ResultError::Gone);
            }
            let a = self.data.read().unwrap();
            let Some(value) = a.cell(row as usize, col) else {
                break false;
            };
            let mut end = (offset + 16_384).min(value.len());
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            let mut hit = false;
            for c in value[offset..end].chars().flat_map(char::to_lowercase) {
                while matched > 0 && c != needle[matched] {
                    matched = prefix[matched - 1];
                }
                if c == needle[matched] {
                    matched += 1;
                }
                if matched == needle.len() {
                    hit = true;
                    break;
                }
            }
            if hit {
                break true;
            }
            if end == value.len() {
                break false;
            }
            offset = end;
        };
        Ok(hit)
    }

    pub(crate) fn copy_rect(&self, rows: RowSpan, cols: ColSpan) -> Result<Vec<ColumnText>, ResultError> {
        if rows.start > rows.end || cols.start > cols.end {
            return Err(ResultError::OutOfRange);
        }
        let a = self.data.read().unwrap();
        if rows.end >= a.row_count() || cols.end as usize >= a.ncols {
            return Err(ResultError::OutOfRange);
        }
        let mut copied = Vec::with_capacity((cols.end - cols.start + 1) as usize);
        for col in cols.start..=cols.end {
            let meta = &self.columns[col as usize];
            let mut cells = Vec::with_capacity((rows.end - rows.start + 1) as usize);
            for row in rows.start..=rows.end {
                cells.push(a.cell(row as usize, col as usize).map(str::to_owned));
            }
            copied.push(ColumnText { display: meta.r#type.display.clone(), category: meta.r#type.category, cells });
        }
        Ok(copied)
    }

    pub fn each_row<E>(&self, mut f: impl FnMut(&[Option<&str>]) -> Result<(), E>) -> Result<(u64, bool), E> {
        let a = self.data.read().unwrap();
        let rows = a.row_count();
        let mut cells = Vec::with_capacity(a.ncols);
        for r in 0..rows as usize {
            cells.clear();
            cells.extend((0..a.ncols).map(|c| a.cell(r, c)));
            f(&cells)?;
        }
        Ok((u64::from(rows), a.truncated.is_some()))
    }
}

fn find_pattern(query: &str) -> (Vec<char>, Vec<usize>) {
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let mut prefix = vec![0; needle.len()];
    let mut matched = 0;
    for i in 1..needle.len() {
        while matched > 0 && needle[i] != needle[matched] {
            matched = prefix[matched - 1];
        }
        if needle[i] == needle[matched] {
            matched += 1;
        }
        prefix[i] = matched;
    }
    (needle, prefix)
}

pub(crate) fn plan_output(
    tab: TabId,
    run_id: RunId,
    kind: DatabaseKind,
    format: PlanFormat,
    schema: &Option<String>,
    connection_id: ConnectionId,
    database: &str,
    cell: &str,
    columns: Vec<ColumnMeta>,
    caps: &Caps,
    started: Instant,
) -> Result<(Arc<ResultSet>, RunOutput), QueryError> {
    if cell.len() as u64 > caps.max_bytes {
        return Err(crate::session::error("The plan exceeds the result size limit."));
    }
    let mut set = ResultSet::new(
        ResultRef { tab_id: tab, run_id, index: 0 },
        DatabaseRef { connection_id, database: database.to_owned() },
        kind,
        columns,
        Origin { rerun: Rerun::Refused("Explain is not re-run."), nth: 0, schema: schema.clone(), page: false },
    );
    set.plan_format = Some(format);
    let set = Arc::new(set);
    set.push_row(std::iter::once(Some(cell)), caps);
    set.set_elapsed(started.elapsed().as_millis().min(u32::MAX as u128) as u32);
    if set.meta().truncated.is_some() {
        return Err(crate::session::error("The plan exceeds the result size limit."));
    }
    let output = RunOutput::Plan { result: set.meta(), format };
    Ok((set, output))
}

impl Arena {
    fn row_count(&self) -> u32 {
        if self.ncols == 0 { 0 } else { (self.cells.len() / self.ncols) as u32 }
    }

    fn cell(&self, row: usize, col: usize) -> Option<&str> {
        let (start, len) = self.cells[row * self.ncols + col];
        (len != NULL).then(|| &self.text[start as usize..(start + len) as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConnectionId, DatabaseRef, SqlTypeRef, RunId, TabId, TypeCategory};

    fn set(ncols: usize) -> ResultSet {
        let columns = (0..ncols)
            .map(|i| ColumnMeta {
                name: format!("c{i}"),
                r#type: SqlTypeRef { display: "text".into(), category: TypeCategory::Text },
                source: None,
            })
            .collect();
        let db = DatabaseRef { connection_id: ConnectionId(uuid::Uuid::nil()), database: "shop".into() };
        let origin = Origin { rerun: Rerun::Ready("select".into()), nth: 0, schema: None, page: false };
        ResultSet::new(ResultRef { tab_id: TabId(uuid::Uuid::nil()), run_id: RunId(1), index: 0 }, db, DatabaseKind::Postgres, columns, origin)
    }

    const NO_CAPS: Caps = Caps { max_rows: u32::MAX, max_bytes: u64::MAX };

    #[test]
    fn find_counts_cells_and_wraps_in_row_major_order() {
        let rs = set(2);
        rs.push_row([Some("foo FOO"), None].into_iter(), &NO_CAPS);
        rs.push_row([Some("food"), Some("FOO")].into_iter(), &NO_CAPS);
        let first = rs.find("foo", ResultFindDirection::First, || true).unwrap();
        assert_eq!(first.count, 3);
        assert_eq!(first.row_count, 2);
        assert_eq!(first.target.unwrap().cell, ResultCell { row: 0, col: 0 });
        let next = rs
            .find(
                "foo",
                ResultFindDirection::Next {
                    anchor: ResultCell { row: 1, col: 1 },
                },
                || true,
            )
            .unwrap();
        assert_eq!(next.target.unwrap().ordinal, 1);
        let previous = rs
            .find(
                "foo",
                ResultFindDirection::Previous {
                    anchor: ResultCell { row: 0, col: 0 },
                },
                || true,
            )
            .unwrap();
        assert_eq!(previous.target.unwrap().ordinal, 3);
        let next = rs
            .find(
                "foo",
                ResultFindDirection::Next {
                    anchor: ResultCell { row: 0, col: 1 },
                },
                || true,
            )
            .unwrap();
        assert_eq!(next.target.unwrap().cell, ResultCell { row: 1, col: 0 });
        assert!(matches!(
            rs.find(
                "x",
                ResultFindDirection::Next {
                    anchor: ResultCell { row: 2, col: 0 }
                },
                || true
            ),
            Err(ResultError::OutOfRange)
        ));
    }

    #[test]
    fn find_full_unicode_values_across_chunks_without_null_or_empty_query_matches() {
        let rs = set(2);
        let long = format!("{}Éİ尾", "x".repeat(16_383));
        rs.push_row([Some(long.as_str()), None].into_iter(), &NO_CAPS);
        assert!(rs.cell_matches(ResultCell { row: 0, col: 0 }, "éİ尾", || true).unwrap());
        assert!(!rs.cell_matches(ResultCell { row: 0, col: 1 }, "NULL", || true).unwrap());
        assert!(matches!(rs.cell_matches(ResultCell { row: 1, col: 0 }, "x", || true), Err(ResultError::OutOfRange)));
        assert_eq!(
            rs.find("éİ尾", ResultFindDirection::First, || true)
                .unwrap()
                .count,
            1
        );
        assert_eq!(
            rs.find("NULL", ResultFindDirection::First, || true)
                .unwrap()
                .count,
            0
        );
        assert_eq!(
            rs.find("", ResultFindDirection::First, || panic!(
                "empty query must not scan"
            ))
            .unwrap()
            .count,
            0
        );
        assert!(matches!(
            rs.find("x", ResultFindDirection::First, || false),
            Err(ResultError::Gone)
        ));
    }

    #[test]
    fn find_preserves_snapshot_row_bound_while_streaming() {
        let rs = set(1);
        let long = format!("{}hit", "x".repeat(32_768));
        rs.push_row([Some(long.as_str())].into_iter(), &NO_CAPS);
        let appended = std::sync::atomic::AtomicUsize::new(0);
        let result = rs
            .find("hit", ResultFindDirection::First, || {
                if appended.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1 {
                    rs.push_row([Some("hit")].into_iter(), &NO_CAPS);
                }
                true
            })
            .unwrap();
        assert_eq!((result.row_count, result.count), (1, 1));
        assert_eq!(rs.meta().row_count, 2);
    }

    #[test]
    fn pages_and_cells_follow_the_arena() {
        let rs = set(2);
        rs.push_row([Some("a"), None].into_iter(), &NO_CAPS);
        rs.push_row([Some(""), Some("b,c")].into_iter(), &NO_CAPS);
        rs.push_row([Some("x"), Some("y")].into_iter(), &NO_CAPS);
        let page = rs.page(1, 10).unwrap();
        assert_eq!(page.start, 1);
        assert_eq!(page.rows, vec![vec![Some("".to_owned()), Some("b,c".to_owned())], vec![Some("x".into()), Some("y".into())]]);
        assert_eq!(rs.page(0, 1).unwrap().rows, vec![vec![Some("a".to_owned()), None]]);
        assert_eq!(rs.page(3, 5).unwrap().rows.len(), 0);
        assert!(rs.page(4, 5).is_none());
        assert_eq!(rs.cell(0, 1), Some(None));
        assert_eq!(rs.cell(1, 1), Some(Some("b,c".into())));
        assert!(rs.cell(3, 0).is_none());
        assert!(rs.cell(0, 2).is_none());
        assert_eq!(rs.meta().row_count, 3);
    }

    #[test]
    fn long_cells_are_cut_in_pages_but_whole_in_cell() {
        let rs = set(1);
        let long = "é".repeat(CELL_CHAR_LIMIT + 5);
        rs.push_row([Some(long.as_str())].into_iter(), &NO_CAPS);
        let page = rs.page(0, 1).unwrap();
        assert_eq!(page.cut, vec![(0, 0)]);
        assert_eq!(page.rows[0][0].as_ref().unwrap().chars().count(), CELL_CHAR_LIMIT);
        assert_eq!(rs.cell(0, 0), Some(Some(long)));
    }

    #[test]
    fn copy_rect_keeps_full_strings_and_rejects_a_span_outside_the_grid() {
        let mut rs = set(2);
        rs.columns[0].r#type = SqlTypeRef { display: "numeric".into(), category: TypeCategory::Numeric };
        let long = "é".repeat(CELL_CHAR_LIMIT + 5);
        rs.push_row([Some("1.50"), None].into_iter(), &NO_CAPS);
        rs.push_row([Some(""), Some(long.as_str())].into_iter(), &NO_CAPS);
        let copied = rs.copy_rect(RowSpan { start: 0, end: 1 }, ColSpan { start: 0, end: 1 }).unwrap();
        assert_eq!(copied[0].display, "numeric");
        assert_eq!(copied[0].category, TypeCategory::Numeric);
        assert_eq!(copied[0].cells, vec![Some("1.50".into()), Some("".into())]);
        assert_eq!(copied[1].cells[0], None);
        assert_eq!(copied[1].cells[1].as_deref(), Some(long.as_str()));
        assert!(matches!(rs.copy_rect(RowSpan { start: 1, end: 0 }, ColSpan { start: 0, end: 0 }), Err(ResultError::OutOfRange)));
        assert!(matches!(rs.copy_rect(RowSpan { start: 0, end: 2 }, ColSpan { start: 0, end: 0 }), Err(ResultError::OutOfRange)));
        assert!(matches!(rs.copy_rect(RowSpan { start: 0, end: 1 }, ColSpan { start: 1, end: 0 }), Err(ResultError::OutOfRange)));
        assert!(matches!(rs.copy_rect(RowSpan { start: 0, end: 1 }, ColSpan { start: 0, end: 2 }), Err(ResultError::OutOfRange)));
    }

    #[test]
    fn row_cap_drains_and_counts() {
        let rs = set(1);
        let caps = Caps { max_rows: 2, max_bytes: u64::MAX };
        for i in 0..5 {
            rs.push_row([Some(i.to_string().as_str())].into_iter(), &caps);
        }
        let meta = rs.meta();
        assert_eq!((meta.row_count, meta.rows_seen, meta.truncated), (2, 5, Some(TruncateReason::RowLimit)));
    }

    #[test]
    fn byte_cap_drops_the_row_that_crossed_it() {
        let rs = set(1);
        let caps = Caps { max_rows: u32::MAX, max_bytes: 5 };
        rs.push_row([Some("abc")].into_iter(), &caps);
        rs.push_row([Some("defg")].into_iter(), &caps);
        rs.push_row([Some("h")].into_iter(), &caps);
        let meta = rs.meta();
        assert_eq!((meta.row_count, meta.rows_seen, meta.truncated), (1, 3, Some(TruncateReason::ByteLimit)));
        assert_eq!(rs.cell(0, 0), Some(Some("abc".into())));
    }
}
