use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use futures_util::StreamExt;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use crate::Shared;
use crate::console::{self, Console, ConsoleInner, Orphaned, Work, statement_line};
use crate::model::*;
use crate::plan;
use crate::results::Caps;
use crate::session::{self, QueryEvent, Session};
use crate::sql::{self, StmtSpan};
use crate::writes;

const OFF_LOST: &str = "Plan settings stayed on after explain. The session was retired so the next batch returns rows.";
const CANCEL_LOST: &str =
    "Cancelled during explain. The session was retired so the next batch returns rows.";

#[derive(Debug)]
pub(crate) struct Prepared {
    analyze: bool,
    batch: String,
    on_sql: &'static str,
    off_sql: &'static str,
    checked: writes::WritesChecked,
}

pub(crate) struct Launch {
    session: Session,
    stop: Arc<AtomicBool>,
    analyze: bool,
    batch: String,
    on_sql: &'static str,
    off_sql: &'static str,
    writes_stay_unasked: bool,
    schema: Option<String>,
    run_id: RunId,
    connection: ConnectionConfig,
    database: String,
    entered: TxnState,
}

pub(crate) fn prepare(
    analyze: bool,
    text: &str,
    spans: &[StmtSpan],
    target: &writes::Target,
    confirmed: Option<&WriteDigest>,
) -> Result<Prepared, RunError> {
    if spans.is_empty() {
        return Err(RunError::NoStatement);
    }
    if spans.len() != 1 {
        return Err(unsupported("Explain one batch."));
    }
    let batch = spans[0].text(text).to_owned();
    sql::tsql::validate_batch(&batch).map_err(unsupported)?;
    if sql::tsql::changes_showplan(&batch) {
        return Err(unsupported("The batch changes the showplan setting."));
    }
    let (on_sql, off_sql) = sets(analyze);
    let checked = if !analyze {
        writes::admit(target, false)
    } else if sql::tsql::rerun_refusal(&batch).is_some() {
        writes::ask(
            target,
            &[batch.as_str()],
            &[on_sql, batch.as_str(), off_sql],
            confirmed,
        )
        .map_err(|confirmation| RunError::ConfirmWrites { confirmation })?
    } else {
        writes::check(target, &[batch.as_str()], confirmed)
            .map_err(|confirmation| RunError::ConfirmWrites { confirmation })?
    };
    Ok(Prepared {
        analyze,
        batch,
        on_sql,
        off_sql,
        checked,
    })
}

pub(crate) fn launch(
    console: &Console,
    inner: &mut ConsoleInner,
    run_id: RunId,
    prepared: Prepared,
    schema: Option<String>,
    connection: ConnectionConfig,
    database: String,
    entered: TxnState,
) -> Result<Launch, RunError> {
    let Prepared {
        analyze,
        batch,
        on_sql,
        off_sql,
        checked,
    } = prepared;
    let writes_stay_unasked = checked.writes_stay_unasked();
    let Some((session, stop)) = console.take_session(inner, run_id, Work::Run(checked)) else {
        return Err(RunError::Busy);
    };
    Ok(Launch {
        session,
        stop,
        analyze,
        batch,
        on_sql,
        off_sql,
        writes_stay_unasked,
        schema,
        run_id,
        connection,
        database,
        entered,
    })
}

pub(crate) async fn run(
    console: Arc<Console>,
    shared: Arc<Shared>,
    launch: Launch,
) -> Result<(), Orphaned> {
    let Launch {
        mut session,
        stop,
        analyze,
        batch,
        on_sql,
        off_sql,
        writes_stay_unasked,
        schema,
        run_id,
        connection,
        database,
        entered,
    } = launch;
    let started = Instant::now();
    let settings = shared.settings();
    let caps = Caps {
        max_rows: settings.max_rows,
        max_bytes: settings.max_result_bytes,
    };

    if let Err(error) = session.batch(on_sql).await {
        let lost = session.closed_reason();
        let end = if stop.load(Ordering::Relaxed) {
            console::PlanEnd::Cancelled
        } else {
            console::PlanEnd::Failed(error)
        };
        return finish(
            &console,
            &shared,
            session,
            run_id,
            &batch,
            &connection,
            &database,
            started,
            entered,
            lost,
            end,
        )
        .await;
    }
    if analyze {
        console
            .edit(&shared, run_id, |inner| {
                inner.invalidate(DatabaseKind::SqlServer, sql::tsql::invalidates(&batch))
            })
            .await?;
    }
    console.output(&shared, run_id, statement_line(0, &batch));
    let read = read_showplan(&mut session, &batch, &stop).await;
    console.output_all(&shared, run_id, console::notice_lines(&mut session));
    if matches!(read, Read::Cancelled) {
        if session.closed_reason().is_none() {
            session.retire(CANCEL_LOST.to_owned());
        }
        let lost = session.closed_reason();
        return finish(
            &console,
            &shared,
            session,
            run_id,
            &batch,
            &connection,
            &database,
            started,
            entered,
            lost,
            console::PlanEnd::Cancelled,
        )
        .await;
    }
    if let Err(error) = session.batch(off_sql).await {
        let (retire, reason) = lost_after_failed_off(session.closed_reason());
        if retire {
            session.retire(reason.clone());
        }
        let lost = Some(reason);
        let end = match read {
            Read::Failed(error) => console::PlanEnd::Failed(error),
            Read::Ready { .. } => console::PlanEnd::Failed(error),
            Read::Cancelled => console::PlanEnd::Cancelled,
        };
        return finish(
            &console,
            &shared,
            session,
            run_id,
            &batch,
            &connection,
            &database,
            started,
            entered,
            lost,
            end,
        )
        .await;
    }
    let txn = session.observed_txn().unwrap_or(entered);
    match read {
        Read::Cancelled => unreachable!("cancel returns before OFF"),
        Read::Failed(error) => {
            finish(
                &console,
                &shared,
                session,
                run_id,
                &batch,
                &connection,
                &database,
                started,
                txn,
                None,
                console::PlanEnd::Failed(error),
            )
            .await
        }
        Read::Ready { cell, columns } => {
            if analyze {
                let write = crate::text::is_write(&batch, DatabaseKind::SqlServer);
                let ended = txn == TxnState::Idle;
                console
                    .edit(&shared, run_id, |inner| {
                        if writes_stay_unasked && write {
                            inner.unasked.add(&batch);
                        }
                        if ended {
                            inner.unasked = WriteList::default();
                        }
                    })
                    .await?;
            }
            let output = match crate::results::plan_output(
                console.tab,
                run_id,
                DatabaseKind::SqlServer,
                PlanFormat::Xml,
                &schema,
                connection.id,
                &database,
                &cell,
                columns,
                &caps,
                started,
            ) {
                Ok((set, output)) => {
                    console.results.lock().unwrap().push(set);
                    output
                }
                Err(error) => {
                    return finish(
                        &console,
                        &shared,
                        session,
                        run_id,
                        &batch,
                        &connection,
                        &database,
                        started,
                        txn,
                        None,
                        console::PlanEnd::Failed(error),
                    )
                    .await;
                }
            };
            finish(
                &console,
                &shared,
                session,
                run_id,
                &batch,
                &connection,
                &database,
                started,
                txn,
                None,
                console::PlanEnd::Ready(output),
            )
            .await
        }
    }
}

enum Read {
    Ready {
        cell: String,
        columns: Vec<ColumnMeta>,
    },
    Failed(QueryError),
    Cancelled,
}

async fn read_showplan(session: &mut Session, sql: &str, stop: &AtomicBool) -> Read {
    let mut stream = session.query(sql);
    let mut columns = Vec::new();
    let mut pending: Option<(String, ColumnMeta)> = None;
    let mut kept: Vec<(String, ColumnMeta)> = Vec::new();
    let mut failure = None;
    while let Some(event) = stream.next().await {
        if stop.load(Ordering::Relaxed) {
            drop(stream);
            return Read::Cancelled;
        }
        match event {
            Err(error) => {
                failure = Some(error);
                break;
            }
            Ok(QueryEvent::Columns(next)) => {
                if let Some(cell) = pending.take() {
                    kept.push(cell);
                }
                columns = next;
            }
            Ok(QueryEvent::Row(row)) if pending.is_none() => {
                if let Some((cell, index)) = plan_cell(&columns, &row) {
                    if let Some(column) = columns.get(index).cloned() {
                        pending = Some((cell, column));
                    }
                }
            }
            Ok(QueryEvent::Complete { .. }) => {
                if let Some(cell) = pending.take() {
                    kept.push(cell);
                }
            }
            Ok(QueryEvent::ServerError(error)) => {
                failure = Some(error);
                break;
            }
            Ok(_) => {}
        }
    }
    drop(stream);
    if let Some(cell) = pending.take() {
        kept.push(cell);
    }
    if stop.load(Ordering::Relaxed) {
        return Read::Cancelled;
    }
    if let Some(error) = failure {
        return if stop.load(Ordering::Relaxed) {
            Read::Cancelled
        } else {
            Read::Failed(error)
        };
    }
    if kept.is_empty() {
        return Read::Failed(session::error("The result is not a plan."));
    }
    let column = kept.last().expect("kept is non-empty").1.clone();
    let cell = kept
        .into_iter()
        .map(|(cell, _)| cell)
        .collect::<Vec<_>>()
        .join("\n");
    Read::Ready {
        cell,
        columns: vec![column],
    }
}

fn plan_cell(columns: &[ColumnMeta], row: &[Option<String>]) -> Option<(String, usize)> {
    if let Some((index, _)) = columns
        .iter()
        .enumerate()
        .find(|(_, column)| column.name.contains("Showplan"))
    {
        let cell = row.get(index).cloned().flatten()?;
        return Some((cell, index));
    }
    row.iter().enumerate().find_map(|(index, cell)| {
        let text = cell.as_ref()?.trim_start();
        if text.starts_with("<ShowPlan") { Some((cell.clone()?, index)) } else { None }
    })
}

async fn finish(
    console: &Console,
    shared: &Arc<Shared>,
    session: Session,
    run_id: RunId,
    sql: &str,
    connection: &ConnectionConfig,
    database: &str,
    started: Instant,
    txn: TxnState,
    lost: Option<String>,
    end: console::PlanEnd,
) -> Result<(), Orphaned> {
    console::finish_plan(
        console,
        shared,
        connection,
        database,
        console::PlanFinish {
            session,
            run_id,
            sql: sql.to_owned(),
            started,
            txn,
            lost,
            end,
        },
    )
    .await
}

fn lost_after_failed_off(already_closed: Option<String>) -> (bool, String) {
    match already_closed {
        Some(reason) => (false, reason),
        None => (true, OFF_LOST.to_owned()),
    }
}

fn sets(analyze: bool) -> (&'static str, &'static str) {
    if analyze {
        ("SET STATISTICS XML ON", "SET STATISTICS XML OFF")
    } else {
        ("SET SHOWPLAN_XML ON", "SET SHOWPLAN_XML OFF")
    }
}

fn unsupported(what: impl Into<String>) -> RunError {
    RunError::Unsupported { what: what.into() }
}

struct XmlNode {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<XmlNode>,
}

pub(crate) fn parse(xml: &str) -> Result<Plan, String> {
    let roots = element_tree(xml)?;
    if roots.is_empty() || roots.iter().any(|root| root.name != "ShowPlanXML") {
        return Err("The result is not a plan.".into());
    }
    let mut statements = Vec::new();
    for root in &roots {
        let mut found = Vec::new();
        collect_statements(root, &mut found);
        for node in found {
            statements.push(statement(node)?);
        }
    }
    if statements.is_empty() {
        return Err("EXPLAIN returned no plan.".into());
    }
    Ok(Plan { statements })
}

fn element_tree(xml: &str) -> Result<Vec<XmlNode>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut roots = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|err| err.to_string())?
        {
            Event::Start(event) => stack.push(node_from(&event)),
            Event::Empty(event) => push_node(&mut stack, &mut roots, node_from(&event)),
            Event::End(_) => {
                let node = stack
                    .pop()
                    .ok_or_else(|| "The result is not a plan.".to_string())?;
                push_node(&mut stack, &mut roots, node);
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if !stack.is_empty() {
        return Err("The result is not a plan.".into());
    }
    Ok(roots)
}

fn node_from(event: &BytesStart<'_>) -> XmlNode {
    XmlNode {
        name: String::from_utf8_lossy(event.local_name().as_ref()).into_owned(),
        attrs: event
            .attributes()
            .flatten()
            .map(|attr| {
                let name = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
                let value = attr
                    .unescape_value()
                    .map(|value| value.into_owned())
                    .unwrap_or_default();
                (name, value)
            })
            .collect(),
        children: Vec::new(),
    }
}

fn push_node(stack: &mut Vec<XmlNode>, roots: &mut Vec<XmlNode>, node: XmlNode) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        roots.push(node);
    }
}

fn is_statement(name: &str) -> bool {
    matches!(
        name,
        "StmtSimple" | "StmtCond" | "StmtCursor" | "StmtReceive" | "StmtUse" | "StmtUseDb"
    )
}

fn collect_statements<'a>(node: &'a XmlNode, out: &mut Vec<&'a XmlNode>) {
    if node.name == "Statements" {
        out.extend(
            node.children
                .iter()
                .filter(|child| is_statement(&child.name)),
        );
        return;
    }
    for child in &node.children {
        collect_statements(child, out);
    }
}

struct Header {
    planning: Option<f64>,
    execution: Option<f64>,
}

fn statement(node: &XmlNode) -> Result<StatementPlan, String> {
    let statement_text = attr(node, "StatementText").unwrap_or("").trim().to_owned();
    let statement_type = attr(node, "StatementType").unwrap_or("Statement");
    let mut header = Header {
        planning: None,
        execution: None,
    };
    let mut relops = Vec::new();
    walk_operators(node, true, &mut header, &mut relops)?;
    let mut nested = Vec::new();
    nested_roots(node, true, &mut nested)?;
    let root = match (relops.len(), nested.len()) {
        (1, 0) => relops.into_iter().next().expect("one relop"),
        (0, 0) => synthetic(statement_type, Vec::new()),
        _ => {
            relops.extend(nested);
            synthetic(statement_type, relops)
        }
    };
    let properties = node
        .attrs
        .iter()
        .filter(|(name, _)| name != "StatementText")
        .map(|(name, value)| PlanProperty {
            name: name.clone(),
            value: value.clone(),
        })
        .collect();
    Ok(StatementPlan {
        planning_time_ms: header.planning,
        execution_time_ms: header.execution,
        statement_text,
        root,
        properties,
    })
}

fn walk_operators(
    node: &XmlNode,
    root: bool,
    header: &mut Header,
    relops: &mut Vec<PlanNode>,
) -> Result<(), String> {
    if !root && is_statement(&node.name) {
        return Ok(());
    }
    if node.name == "QueryPlan" {
        if header.planning.is_none() {
            header.planning = attr_f64(node, "CompileTime");
        }
        if header.execution.is_none() {
            header.execution = node
                .children
                .iter()
                .find(|child| child.name == "QueryTimeStats")
                .and_then(|stats| attr_f64(stats, "ElapsedTime"));
        }
    }
    if node.name == "RelOp" {
        relops.push(relop_node(node)?);
        return Ok(());
    }
    for child in &node.children {
        walk_operators(child, false, header, relops)?;
    }
    Ok(())
}

fn nested_roots(node: &XmlNode, root: bool, out: &mut Vec<PlanNode>) -> Result<(), String> {
    if !root && is_statement(&node.name) {
        out.push(statement(node)?.root);
        return Ok(());
    }
    for child in &node.children {
        nested_roots(child, false, out)?;
    }
    Ok(())
}

fn relop_node(node: &XmlNode) -> Result<PlanNode, String> {
    let physical =
        attr(node, "PhysicalOp").ok_or_else(|| "a plan node has no PhysicalOp".to_string())?;
    let logical = attr(node, "LogicalOp");
    let operation = match logical {
        Some(logical) if logical != physical => format!("{logical} {physical}"),
        _ => physical.to_owned(),
    };
    let children = child_relops(node)?;
    let counters = counters_of(node);
    let plan_rows = attr_f64(node, "EstimateRows").unwrap_or(0.0);
    let total_time_ms = counters.as_ref().map(|counters| counters.elapsed);
    Ok(PlanNode {
        operation,
        detail: object_detail(node),
        startup_cost: 0.0,
        total_cost: attr_f64(node, "EstimatedTotalSubtreeCost").unwrap_or(0.0),
        plan_rows,
        actual_rows: counters.as_ref().map(|counters| counters.rows),
        loops: counters.as_ref().map(|counters| counters.executions),
        total_time_ms,
        self_time_ms: plan::self_time(total_time_ms, &children),
        diverges: plan::diverges(plan_rows, counters.as_ref().map(|counters| counters.rows)),
        buffers: None,
        properties: node
            .attrs
            .iter()
            .filter(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "PhysicalOp" | "EstimateRows" | "EstimatedTotalSubtreeCost" | "NodeId"
                )
            })
            .map(|(name, value)| PlanProperty {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        children,
    })
}

fn child_relops(relop: &XmlNode) -> Result<Vec<PlanNode>, String> {
    let mut children = Vec::new();
    fn walk(node: &XmlNode, out: &mut Vec<PlanNode>) -> Result<(), String> {
        if node.name == "RelOp" {
            out.push(relop_node(node)?);
            return Ok(());
        }
        for child in &node.children {
            walk(child, out)?;
        }
        Ok(())
    }
    for child in &relop.children {
        walk(child, &mut children)?;
    }
    Ok(children)
}

struct Counters {
    rows: f64,
    executions: f64,
    elapsed: f64,
}

fn counters_of(relop: &XmlNode) -> Option<Counters> {
    let mut found = None;
    fn walk(node: &XmlNode, found: &mut Option<Counters>) {
        if node.name == "RelOp" {
            return;
        }
        if node.name == "RunTimeCountersPerThread" {
            let slot = found.get_or_insert(Counters {
                rows: 0.0,
                executions: 0.0,
                elapsed: 0.0,
            });
            slot.rows += attr_f64(node, "ActualRows").unwrap_or(0.0);
            slot.executions += attr_f64(node, "ActualExecutions").unwrap_or(0.0);
            slot.elapsed += attr_f64(node, "ActualElapsedms").unwrap_or(0.0);
        }
        for child in &node.children {
            walk(child, found);
        }
    }
    for child in &relop.children {
        walk(child, &mut found);
    }
    found
}

fn object_detail(relop: &XmlNode) -> String {
    let Some(object) = relop.children.iter().find_map(first_object) else {
        return String::new();
    };
    let schema = attr(object, "Schema").map(unbracket);
    let table = attr(object, "Table").map(unbracket);
    let index = attr(object, "Index").map(unbracket);
    let mut text = String::new();
    if let Some(table) = table {
        text.push_str("on ");
        if let Some(schema) = schema {
            text.push_str(&schema);
            text.push('.');
        }
        text.push_str(&table);
    }
    if let Some(index) = index {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str("using ");
        text.push_str(&index);
    }
    text
}

fn first_object(node: &XmlNode) -> Option<&XmlNode> {
    if node.name == "RelOp" {
        return None;
    }
    if node.name == "Object" {
        return Some(node);
    }
    node.children.iter().find_map(first_object)
}

fn synthetic(operation: &str, children: Vec<PlanNode>) -> PlanNode {
    PlanNode {
        operation: operation.to_owned(),
        detail: String::new(),
        startup_cost: 0.0,
        total_cost: 0.0,
        plan_rows: 0.0,
        actual_rows: None,
        loops: None,
        total_time_ms: None,
        self_time_ms: plan::self_time(None, &children),
        diverges: false,
        buffers: None,
        properties: Vec::new(),
        children,
    }
}

fn attr<'a>(node: &'a XmlNode, name: &str) -> Option<&'a str> {
    node.attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn attr_f64(node: &XmlNode, name: &str) -> Option<f64> {
    attr(node, name)?.parse().ok()
}

fn unbracket(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() >= 2 {
        trimmed[1..trimmed.len() - 1].replace("]]", "]")
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const FIXTURE: &str = r#"<ShowPlanXML xmlns="http://schemas.microsoft.com/sqlserver/2004/07/showplan" Version="1.5">
  <BatchSequence>
    <Batch>
      <Statements>
        <StmtSimple StatementText="SELECT 1" StatementType="SELECT">
          <QueryPlan>
            <RelOp NodeId="0" PhysicalOp="Nested Loops" LogicalOp="Inner Join" EstimateRows="1" EstimatedTotalSubtreeCost="0.01">
              <RunTimeInformation>
                <RunTimeCountersPerThread Thread="0" ActualRows="1" ActualElapsedms="0.2" ActualExecutions="1" />
                <RunTimeCountersPerThread Thread="1" ActualRows="2" ActualElapsedms="0.3" ActualExecutions="1" />
              </RunTimeInformation>
              <NestedLoops>
                <RelOp NodeId="1" PhysicalOp="Constant Scan" LogicalOp="Constant Scan" EstimateRows="1" EstimatedTotalSubtreeCost="0.000001">
                  <RunTimeInformation>
                    <RunTimeCountersPerThread Thread="0" ActualRows="1" ActualElapsedms="0.1" ActualExecutions="1" />
                  </RunTimeInformation>
                </RelOp>
              </NestedLoops>
            </RelOp>
          </QueryPlan>
        </StmtSimple>
        <StmtSimple StatementText="SELECT 2" StatementType="SELECT">
          <QueryPlan>
            <RelOp NodeId="0" PhysicalOp="Clustered Index Scan" LogicalOp="Clustered Index Scan" EstimateRows="10" EstimatedTotalSubtreeCost="0.003">
              <IndexScan>
                <Object Database="[shop]" Schema="[dbo]" Table="[orders]" Index="[pk_orders]" />
              </IndexScan>
            </RelOp>
          </QueryPlan>
        </StmtSimple>
      </Statements>
    </Batch>
  </BatchSequence>
</ShowPlanXML>"#;

    fn near(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("value");
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn a_two_statement_showplan_becomes_two_roots() {
        let plan = parse(FIXTURE).unwrap();
        assert_eq!(plan.statements.len(), 2);
        let first = &plan.statements[0];
        assert_eq!(first.statement_text, "SELECT 1");
        assert_eq!(first.root.operation, "Inner Join Nested Loops");
        assert_eq!(first.root.plan_rows, 1.0);
        near(first.root.actual_rows, 3.0);
        near(first.root.total_time_ms, 0.5);
        near(first.root.loops, 2.0);
        near(first.root.self_time_ms, 0.4);
        assert_eq!(first.root.children.len(), 1);
        assert_eq!(first.root.children[0].operation, "Constant Scan");
        near(first.root.children[0].actual_rows, 1.0);
        let second = &plan.statements[1];
        assert_eq!(second.statement_text, "SELECT 2");
        assert_eq!(second.root.operation, "Clustered Index Scan");
        assert_eq!(second.root.detail, "on dbo.orders using pk_orders");
        assert_eq!(second.root.actual_rows, None);
    }

    #[test]
    fn several_relops_share_a_statement_root_and_the_header_uses_query_time() {
        let xml = r#"<ShowPlanXML><BatchSequence><Batch><Statements>
          <StmtSimple StatementText="SELECT 1" StatementType="SELECT">
            <QueryPlan CompileTime="2">
              <QueryTimeStats ElapsedTime="5" />
              <RelOp PhysicalOp="A" LogicalOp="Left" EstimateRows="1" EstimatedTotalSubtreeCost="1" />
              <RelOp PhysicalOp="B" EstimateRows="2" EstimatedTotalSubtreeCost="2" />
            </QueryPlan>
          </StmtSimple>
        </Statements></Batch></BatchSequence></ShowPlanXML>"#;
        let plan = parse(xml).unwrap();
        let statement = &plan.statements[0];
        assert_eq!(statement.planning_time_ms, Some(2.0));
        assert_eq!(statement.execution_time_ms, Some(5.0));
        assert_eq!(statement.root.operation, "SELECT");
        assert_eq!(statement.root.children.len(), 2);
        assert_eq!(statement.root.children[0].operation, "Left A");
        assert_eq!(statement.root.children[1].operation, "B");
    }

    #[test]
    fn a_statement_without_a_relop_still_has_a_root() {
        let xml = r#"<ShowPlanXML><BatchSequence><Batch><Statements>
          <StmtUse StatementText="USE shop" StatementType="USE DATABASE" />
        </Statements></Batch></BatchSequence></ShowPlanXML>"#;
        let plan = parse(xml).unwrap();
        assert_eq!(plan.statements[0].statement_text, "USE shop");
        assert_eq!(plan.statements[0].root.operation, "USE DATABASE");
        assert!(plan.statements[0].root.children.is_empty());
    }

    #[test]
    fn a_document_that_is_not_a_showplan_is_refused() {
        assert_eq!(parse("<orders/>").unwrap_err(), "The result is not a plan.");
        assert_eq!(
            parse(r#"<ShowPlanXML><BatchSequence><Batch><Statements/></Batch></BatchSequence></ShowPlanXML>"#).unwrap_err(),
            "EXPLAIN returned no plan."
        );
    }

    #[test]
    fn two_showplan_documents_become_one_plan() {
        let plan = parse(&format!("{FIXTURE}\n{FIXTURE}")).unwrap();
        assert_eq!(plan.statements.len(), 4);
    }

    #[test]
    fn a_failed_off_retires_a_live_session() {
        let (retire, reason) = lost_after_failed_off(None);
        assert!(retire);
        assert_eq!(reason, OFF_LOST);
        let (retire, reason) = lost_after_failed_off(Some("Cancelled".into()));
        assert!(!retire);
        assert_eq!(reason, "Cancelled");
    }

    fn column(name: &str) -> ColumnMeta {
        ColumnMeta {
            name: name.into(),
            r#type: SqlTypeRef {
                display: "xml".into(),
                category: TypeCategory::Text,
            },
            source: None,
        }
    }

    #[test]
    fn the_showplan_column_is_the_plan_cell() {
        let columns = vec![
            column("note"),
            column("Microsoft SQL Server 2005 XML Showplan"),
        ];
        let row = vec![Some("<orders/>".into()), Some("<ShowPlanXML/>".into())];
        let (cell, index) = plan_cell(&columns, &row).unwrap();
        assert_eq!((index, cell.as_str()), (1, "<ShowPlanXML/>"));
        assert!(plan_cell(&[column("note")], &[Some("<orders/>".into())]).is_none());
        let (cell, index) =
            plan_cell(&[column("xml")], &[Some("  <ShowPlanXML/>".into())]).unwrap();
        assert_eq!((index, cell.as_str()), (0, "  <ShowPlanXML/>"));
    }

    fn span_of(text: &str) -> StmtSpan {
        StmtSpan {
            start: 0,
            end: text.len(),
        }
    }

    struct TargetOwner {
        saved: ConnectionConfig,
        console: ConsoleConfigView,
        unasked: WriteList,
    }

    impl TargetOwner {
        fn new(environment: Option<Environment>) -> Self {
            let id = ConnectionId(Uuid::from_u128(1));
            Self {
                saved: ConnectionConfig {
                    database_kind: DatabaseKind::SqlServer,
                    id,
                    name: "dev".into(),
                    host: "db".into(),
                    port: 1433,
                    database: "shop".into(),
                    user: "sa".into(),
                    ssl_mode: SslMode::Require,
                    ca_cert_path: None,
                    has_password: false,
                    color: None,
                    ssh: None,
                    environment,
                    sql_server_auth: None,
                },
                console: ConsoleConfigView {
                    connection_id: id,
                    database: "shop".into(),
                    schema: None,
                    mode: TxnMode::AutoCommit,
                },
                unasked: WriteList::default(),
            }
        }

        fn target(&self) -> writes::Target<'_> {
            writes::Target {
                saved: &self.saved,
                opened: &self.saved,
                console: &self.console,
                txn: TxnState::Idle,
                unasked: &self.unasked,
            }
        }
    }

    fn expect_unsupported(err: RunError, what: &str) {
        match err {
            RunError::Unsupported { what: got } => assert_eq!(got, what),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn prepare_explains_one_batch_and_asks_before_an_analyzed_write() {
        let owner = TargetOwner::new(None);
        let target = owner.target();
        let select = "select 1";
        assert!(prepare(false, select, &[span_of(select)], &target, None).is_ok());
        let update = "UPDATE dbo.orders SET status = status WHERE 1 = 0";
        assert!(
            prepare(false, update, &[span_of(update)], &target, None).is_ok(),
            "plain explain does not ask"
        );
        let asked = prepare(true, update, &[span_of(update)], &target, None).unwrap_err();
        let RunError::ConfirmWrites { confirmation } = asked else {
            panic!("analyze update should ask, got {asked:?}")
        };
        assert!(!confirmation.on_production);
        assert_eq!(confirmation.sends.statements, [update]);
        assert!(
            prepare(
                true,
                update,
                &[span_of(update)],
                &target,
                Some(&confirmation.digest)
            )
            .is_ok()
        );
        let changed = "UPDATE dbo.orders SET status = status WHERE 1 = 1";
        assert!(matches!(
            prepare(
                true,
                changed,
                &[span_of(changed)],
                &target,
                Some(&confirmation.digest)
            ),
            Err(RunError::ConfirmWrites { .. })
        ));
        assert!(
            prepare(true, select, &[span_of(select)], &target, None).is_ok(),
            "a safe analyze does not ask off Production"
        );

        let two = [
            StmtSpan { start: 0, end: 8 },
            StmtSpan { start: 9, end: 17 },
        ];
        expect_unsupported(
            prepare(false, "select 1\nselect 2", &two, &target, None).unwrap_err(),
            "Explain one batch.",
        );
        let use_db = "USE other";
        expect_unsupported(
            prepare(false, use_db, &[span_of(use_db)], &target, None).unwrap_err(),
            "USE is unsupported. Select the database through the console instead",
        );
        let sets_plan = "SET SHOWPLAN_XML OFF; select 1";
        expect_unsupported(
            prepare(false, sets_plan, &[span_of(sets_plan)], &target, None).unwrap_err(),
            "The batch changes the showplan setting.",
        );
        let commented = "-- SET SHOWPLAN_XML ON\nselect 1";
        assert!(prepare(false, commented, &[span_of(commented)], &target, None).is_ok());
        let quoted = "select 'SHOWPLAN_XML'";
        assert!(prepare(false, quoted, &[span_of(quoted)], &target, None).is_ok());
        assert!(matches!(
            prepare(false, select, &[], &target, None),
            Err(RunError::NoStatement)
        ));
        assert!(
            prepare(
                false,
                select,
                &[span_of(select)],
                &writes::Target {
                    txn: TxnState::Open,
                    ..target
                },
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn a_safe_analyze_on_production_still_asks_when_writes_are_held() {
        let mut owner = TargetOwner::new(Some(Environment::Production));
        owner.unasked.add("update t set n = 1");
        let target = owner.target();
        let select = "select 1";
        let asked = prepare(true, select, &[span_of(select)], &target, None).unwrap_err();
        let RunError::ConfirmWrites { confirmation } = asked else {
            panic!("{asked:?}")
        };
        assert!(confirmation.on_production);
        assert_eq!(confirmation.holds.count, 1);
        let update = "UPDATE dbo.orders SET status = status";
        assert!(
            prepare(false, update, &[span_of(update)], &target, None).is_ok(),
            "plain explain does not ask and does not list the update"
        );
        let show = match prepare(true, update, &[span_of(update)], &target, None) {
            Err(RunError::ConfirmWrites { confirmation }) => confirmation.digest,
            other => panic!("{other:?}"),
        };
        let normal = writes::check(&target, &[update], None).unwrap_err().digest;
        assert_ne!(
            show, normal,
            "a normal run's digest must not accept Explain Analyze"
        );
    }
}
