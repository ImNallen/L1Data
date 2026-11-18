use serde::Deserialize;

use super::reply::{fence_close, fence_open};
use crate::model::{AskOutcome, AskSource, AssistantAccess};

pub(crate) const FENCE: &str = "ask-edit";
pub(crate) const SYSTEM_PROMPT: &str = include_str!("system_prompt_ask.md");

#[derive(Debug, Clone)]
pub(crate) enum Purpose {
    Chat,
    Ask { source: AskSource, schema: Option<String> },
}

impl Purpose {
    pub(crate) fn access(&self, access: AssistantAccess) -> AssistantAccess {
        match self {
            Self::Chat => access,
            Self::Ask { .. } => AssistantAccess::SchemaOnly,
        }
    }

    pub(crate) fn source(&self) -> Option<&AskSource> {
        match self {
            Self::Chat => None,
            Self::Ask { source, .. } => Some(source),
        }
    }

    pub(crate) fn is_ask(&self) -> bool {
        matches!(self, Self::Ask { .. })
    }
}

pub(crate) fn source_text(source: &AskSource) -> String {
    // JSON preserves the exact capture, including newlines and an intentionally empty draft.
    format!("Explicit SQL attachment:\n{}", serde_json::to_string(source).expect("SQL attachment serializes"))
}

pub(crate) fn outcome(text: &str) -> AskOutcome {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Replacement {
        sql: String,
    }

    let invalid = || AskOutcome::Invalid {
        message: "The assistant did not return one complete SQL proposal. Try asking again.".into(),
    };
    let mut replacement = None;
    let mut lines = text.split_inclusive('\n');
    while let Some(line) = lines.next() {
        let Some((ticks, info)) = fence_open(line.trim_end_matches(['\r', '\n'])) else { continue };
        let mut body = String::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if fence_close(inner.trim_end_matches(['\r', '\n']), ticks) {
                closed = true;
                break;
            }
            body.push_str(inner);
        }
        if !info.eq_ignore_ascii_case(FENCE) {
            continue;
        }
        if !closed || replacement.is_some() || line.trim().trim_start_matches('`').trim() != FENCE {
            return invalid();
        }
        let Ok(Replacement { sql }) = serde_json::from_str(&body) else { return invalid() };
        replacement = Some(sql);
    }
    match replacement {
        Some(sql) => AskOutcome::Replacement { sql },
        None if super::reply::parse_ask_text(text, true).blocks.is_empty() => invalid(),
        None => AskOutcome::Answer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposals_preserve_exact_sql_and_empty_deletion() {
        for sql in ["", "  SELECT 1;\r\n\t", "SELECT '```';\n"] {
            let text = format!("```ask-edit\r\n{}\r\n```", serde_json::json!({"sql": sql}));
            assert_eq!(outcome(&text), AskOutcome::Replacement { sql: sql.into() });
        }
        assert_eq!(outcome("Example:\n```sql\nSELECT 1\n```"), AskOutcome::Answer);
    }

    #[test]
    fn proposals_need_one_closed_strict_json_envelope() {
        for text in [
            "",
            " \n",
            "```ask-edit\n{\"sql\":\"x\"}",
            "```ask-edit\n{\"sql\":\"x\",\"sql\":\"y\"}\n```",
            "```ask-edit\n{\"sql\":\"x\",\"extra\":true}\n```",
            "```ask-edit\n{\"sql\":null}\n```",
            "```ask-edit\n{\"sql\":\"x\"}\n```\n```ask-edit\n{\"sql\":\"y\"}\n```",
            "```ask-edit\n{\"sql\":\"x\"}\n```\n```ask-edit\n",
            "```ask-edit extra\n{\"sql\":\"x\"}\n```",
        ] {
            assert!(matches!(outcome(text), AskOutcome::Invalid { .. }), "{text:?}");
        }
    }
}
