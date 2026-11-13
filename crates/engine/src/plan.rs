use serde_json::{Map, Value};

use crate::model::{Plan, PlanBuffers, PlanNode, PlanProperty, StatementPlan};

pub(crate) fn parse(json: &str) -> Result<Plan, String> {
    let values: Vec<Value> = serde_json::from_str(json).map_err(|err| err.to_string())?;
    if values.is_empty() {
        return Err("EXPLAIN returned no plan.".to_string());
    }
    let mut statements = Vec::with_capacity(values.len());
    for value in values {
        let obj = serde_json::from_value::<Map<String, Value>>(value).map_err(|err| err.to_string())?;
        statements.push(statement(obj)?);
    }
    Ok(Plan { statements })
}

fn statement(mut obj: Map<String, Value>) -> Result<StatementPlan, String> {
    let planning_time_ms = take_opt_f64(&mut obj, "Planning Time")?;
    let execution_time_ms = take_opt_f64(&mut obj, "Execution Time")?;
    let plan_value = obj.remove("Plan").ok_or_else(|| "a plan has no Plan".to_string())?;
    let root_obj =
        serde_json::from_value::<Map<String, Value>>(plan_value).map_err(|_| "a plan has no Plan".to_string())?;
    let root = node(&root_obj)?;
    let properties = properties_of(&obj, &[]);
    Ok(StatementPlan { planning_time_ms, execution_time_ms, statement_text: String::new(), root, properties })
}

fn node(obj: &Map<String, Value>) -> Result<PlanNode, String> {
    let node_type = text_field(obj, "Node Type").ok_or_else(|| "a plan node has no Node Type".to_string())?;
    let operation = match text_field(obj, "Join Type") {
        Some(join) => format!("{join} {node_type}"),
        None => node_type.to_string(),
    };
    let children = children(obj)?;
    let startup_cost = f64_or_zero(obj, "Startup Cost")?;
    let total_cost = f64_or_zero(obj, "Total Cost")?;
    let plan_rows = f64_or_zero(obj, "Plan Rows")?;
    let actual_rows = opt_f64(obj, "Actual Rows")?;
    let loops = opt_f64(obj, "Actual Loops")?;
    let actual_total = opt_f64(obj, "Actual Total Time")?;
    // PostgreSQL reports actual time per loop, not summed across loops.
    let total_time_ms = actual_total.map(|time| time * loops.unwrap_or(1.0));
    let self_time_ms = self_time(total_time_ms, &children);
    Ok(PlanNode {
        operation,
        detail: detail(obj),
        startup_cost,
        total_cost,
        plan_rows,
        actual_rows,
        loops,
        total_time_ms,
        self_time_ms,
        diverges: diverges(plan_rows, actual_rows),
        buffers: buffers(obj),
        children,
        properties: properties_of(obj, LIFTED_NODE_KEYS),
    })
}

fn children(obj: &Map<String, Value>) -> Result<Vec<PlanNode>, String> {
    let Some(value) = obj.get("Plans") else {
        return Ok(Vec::new());
    };
    let list = serde_json::from_value::<Vec<Value>>(value.clone()).map_err(|err| err.to_string())?;
    let mut nodes = Vec::with_capacity(list.len());
    for item in list {
        let child = serde_json::from_value::<Map<String, Value>>(item).map_err(|err| err.to_string())?;
        nodes.push(node(&child)?);
    }
    Ok(nodes)
}

fn detail(obj: &Map<String, Value>) -> String {
    let Some(relation) = text_field(obj, "Relation Name") else {
        return ["Index Name", "CTE Name", "Subplan Name", "Function Name"]
            .into_iter()
            .find_map(|key| text_field(obj, key))
            .unwrap_or("")
            .to_string();
    };
    let mut text = String::from("on ");
    if let Some(schema) = text_field(obj, "Schema") {
        text.push_str(schema);
        text.push('.');
    }
    text.push_str(relation);
    if let Some(alias) = text_field(obj, "Alias") {
        if alias != relation {
            text.push(' ');
            text.push_str(alias);
        }
    }
    if let Some(index) = text_field(obj, "Index Name") {
        text.push_str(" using ");
        text.push_str(index);
    }
    text
}

pub(crate) fn self_time(inclusive: Option<f64>, children: &[PlanNode]) -> Option<f64> {
    let inclusive = inclusive?;
    let spent: f64 = children.iter().filter_map(|child| child.total_time_ms).sum();
    // Timing noise can put the children above the parent.
    Some((inclusive - spent).max(0.0))
}

pub(crate) fn diverges(plan_rows: f64, actual_rows: Option<f64>) -> bool {
    let Some(actual) = actual_rows else {
        return false;
    };
    if plan_rows == 0.0 && actual == 0.0 {
        return false;
    }
    if plan_rows == 0.0 || actual == 0.0 {
        return true;
    }
    let ratio = (actual / plan_rows).abs();
    ratio >= 10.0 || ratio <= 0.1
}

const BUFFER_KEYS: [&str; 10] = [
    "Shared Hit Blocks",
    "Shared Read Blocks",
    "Shared Dirtied Blocks",
    "Shared Written Blocks",
    "Local Hit Blocks",
    "Local Read Blocks",
    "Local Dirtied Blocks",
    "Local Written Blocks",
    "Temp Read Blocks",
    "Temp Written Blocks",
];

const LIFTED_NODE_KEYS: &[&str] = &[
    "Node Type",
    "Join Type",
    "Relation Name",
    "Schema",
    "Alias",
    "Index Name",
    "CTE Name",
    "Subplan Name",
    "Function Name",
    "Startup Cost",
    "Total Cost",
    "Plan Rows",
    "Plan Width",
    "Actual Rows",
    "Actual Loops",
    "Actual Startup Time",
    "Actual Total Time",
    "Shared Hit Blocks",
    "Shared Read Blocks",
    "Shared Dirtied Blocks",
    "Shared Written Blocks",
    "Local Hit Blocks",
    "Local Read Blocks",
    "Local Dirtied Blocks",
    "Local Written Blocks",
    "Temp Read Blocks",
    "Temp Written Blocks",
    "Plans",
];

fn buffers(obj: &Map<String, Value>) -> Option<PlanBuffers> {
    if !BUFFER_KEYS.iter().any(|key| obj.contains_key(*key)) {
        return None;
    }
    Some(PlanBuffers {
        shared_hit: block(obj, "Shared Hit Blocks"),
        shared_read: block(obj, "Shared Read Blocks"),
        shared_dirtied: block(obj, "Shared Dirtied Blocks"),
        shared_written: block(obj, "Shared Written Blocks"),
        local_hit: block(obj, "Local Hit Blocks"),
        local_read: block(obj, "Local Read Blocks"),
        local_dirtied: block(obj, "Local Dirtied Blocks"),
        local_written: block(obj, "Local Written Blocks"),
        temp_read: block(obj, "Temp Read Blocks"),
        temp_written: block(obj, "Temp Written Blocks"),
    })
}

fn block(obj: &Map<String, Value>, key: &str) -> i64 {
    obj.get(key).map(json_i64).unwrap_or(0)
}

fn json_i64(value: &Value) -> i64 {
    value.as_i64().or_else(|| value.as_f64().map(|number| number as i64)).unwrap_or(0)
}

fn properties_of(obj: &Map<String, Value>, lifted: &[&str]) -> Vec<PlanProperty> {
    obj.iter()
        .filter(|(key, _)| !lifted.iter().any(|name| name == key))
        .map(|(key, value)| PlanProperty { name: key.clone(), value: show(value) })
        .collect()
}

fn show(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn text_field<'a>(obj: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(Value::as_str)
}

fn opt_f64(obj: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match obj.get(key) {
        None => Ok(None),
        Some(value) => serde_json::from_value(value.clone()).map(Some).map_err(|err| err.to_string()),
    }
}

fn f64_or_zero(obj: &Map<String, Value>, key: &str) -> Result<f64, String> {
    Ok(opt_f64(obj, key)?.unwrap_or(0.0))
}

fn take_opt_f64(obj: &mut Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match obj.remove(key) {
        None => Ok(None),
        Some(value) => serde_json::from_value(value).map(Some).map_err(|err| err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_explain_document() {
        let json = r#"[
          {
            "Planning Time": 0.5,
            "Execution Time": 1.5,
            "Whatever": "kept",
            "Plan": {
              "Node Type": "Limit",
              "Startup Cost": 0,
              "Total Cost": 3.5,
              "Plan Rows": 1,
              "Plan Width": 8,
              "Actual Rows": 10,
              "Actual Loops": 2,
              "Actual Startup Time": 0.01,
              "Actual Total Time": 3,
              "Shared Hit Blocks": 7,
              "Shared Read Blocks": 1,
              "Temp Written Blocks": 3,
              "Output": ["id"],
              "Plans": [
                {
                  "Node Type": "Index Scan",
                  "Relation Name": "orders",
                  "Schema": "public",
                  "Alias": "o",
                  "Index Name": "orders_pkey",
                  "Startup Cost": 0.1,
                  "Total Cost": 1.1,
                  "Plan Rows": 1,
                  "Actual Rows": 1,
                  "Actual Loops": 2,
                  "Actual Total Time": 1,
                  "Filter": "(id = 1)",
                  "Plan Width": 4
                }
              ]
            }
          },
          {
            "Plan": {
              "Node Type": "Seq Scan",
              "Startup Cost": 0.25,
              "Total Cost": 2.5,
              "Plan Rows": 3,
              "Parallel Aware": false
            }
          },
          {
            "Plan": {
              "Node Type": "Hash Join",
              "Join Type": "Inner",
              "Startup Cost": 1,
              "Total Cost": 2,
              "Plan Rows": 0,
              "Actual Rows": 0
            }
          },
          {
            "Plan": {
              "Node Type": "CTE Scan",
              "CTE Name": "rows",
              "Startup Cost": 0,
              "Total Cost": 0,
              "Plan Rows": 1
            }
          }
        ]"#;
        let plan = parse(json).unwrap();
        assert_eq!(plan.statements.len(), 4);

        let timed = &plan.statements[0];
        assert_eq!(timed.statement_text, "");
        assert_eq!(timed.planning_time_ms, Some(0.5));
        assert_eq!(timed.execution_time_ms, Some(1.5));
        assert_eq!(timed.properties, vec![PlanProperty { name: "Whatever".to_string(), value: "kept".to_string() }]);
        let parent = &timed.root;
        assert_eq!(parent.total_time_ms, Some(6.0));
        assert_eq!(parent.self_time_ms, Some(4.0));
        assert_eq!(parent.loops, Some(2.0));
        assert!(parent.diverges);
        assert_eq!(
            parent.buffers,
            Some(PlanBuffers {
                shared_hit: 7,
                shared_read: 1,
                shared_dirtied: 0,
                shared_written: 0,
                local_hit: 0,
                local_read: 0,
                local_dirtied: 0,
                local_written: 0,
                temp_read: 0,
                temp_written: 3,
            })
        );
        assert_eq!(parent.properties, vec![PlanProperty { name: "Output".to_string(), value: "[\"id\"]".to_string() }]);
        assert_eq!(parent.children.len(), 1);
        let child = &parent.children[0];
        assert_eq!(child.operation, "Index Scan");
        assert_eq!(child.detail, "on public.orders o using orders_pkey");
        assert_eq!(child.total_time_ms, Some(2.0));
        assert_eq!(child.self_time_ms, Some(2.0));
        assert_eq!(child.buffers, None);
        assert_eq!(child.properties, vec![PlanProperty { name: "Filter".to_string(), value: "(id = 1)".to_string() }]);

        let costs_only = &plan.statements[1];
        assert_eq!(costs_only.planning_time_ms, None);
        assert_eq!(costs_only.execution_time_ms, None);
        assert_eq!(costs_only.root.self_time_ms, None);
        assert!(!costs_only.root.diverges);
        assert_eq!(
            costs_only.root.properties,
            vec![PlanProperty { name: "Parallel Aware".to_string(), value: "false".to_string() }]
        );

        let join = &plan.statements[2];
        assert_eq!(join.planning_time_ms, None);
        assert_eq!(join.root.operation, "Inner Hash Join");
        assert!(!join.root.diverges);

        assert_eq!(plan.statements[3].root.detail, "rows");
    }

    #[test]
    fn empty_array_is_not_a_plan() {
        assert_eq!(parse("[]").unwrap_err(), "EXPLAIN returned no plan.");
    }

    #[test]
    fn non_array_is_the_serde_error() {
        let json = "{}";
        let expected = serde_json::from_str::<Vec<Value>>(json).unwrap_err().to_string();
        assert_eq!(parse(json).unwrap_err(), expected);
    }
}
