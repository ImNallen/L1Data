use crate::model::ObjectScript;

pub mod postgres;
pub mod tsql;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Ddl {
    pub text: String,
    pub warnings: Vec<String>,
}

impl Ddl {
    pub fn complete(text: String) -> Ddl {
        Ddl { text, warnings: vec![] }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Privilege {
    pub name: String,
    pub column: Option<String>,
    pub grantee: String,
    pub change: Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Granted,
    GrantedWithGrantOption,
    Revoked,
    Denied,
}

pub fn privileges(object: &str, list: &[Privilege]) -> Option<String> {
    let list: Vec<String> = list
        .iter()
        .map(|g| {
            let column = g.column.as_ref().map_or(String::new(), |c| format!(" ({c})"));
            let change = match g.change {
                Change::Granted => "granted to",
                Change::GrantedWithGrantOption => "granted with grant option to",
                Change::Revoked => "revoked from",
                Change::Denied => "denied to",
            };
            format!("{}{column} {change} {}", g.name, g.grantee)
        })
        .collect();
    (!list.is_empty()).then(|| format!("privileges on {object} ({})", list.join(", ")))
}

pub fn object_script(name: &str, ddl: Ddl) -> ObjectScript {
    let warnings: Vec<String> = ddl.warnings.iter().map(|w| w.replace(['\r', '\n'], " ")).collect();
    let mut text: String = warnings.iter().map(|w| format!("-- L1 Data could not script: {w}\n")).collect();
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&ddl.text);
    ObjectScript { title: format!("{name}.sql"), text, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_head_the_text_one_comment_line_each() {
        let s = object_script(
            "t",
            Ddl {
                text: "CREATE TABLE t ();\n".into(),
                warnings: vec!["privileges on public.t".into(), "rules on public.\"a\nb\"".into()],
            },
        );
        assert_eq!(s.title, "t.sql");
        assert_eq!(
            s.text,
            "-- L1 Data could not script: privileges on public.t\n-- L1 Data could not script: rules on public.\"a b\"\n\nCREATE TABLE t ();\n"
        );
        assert_eq!(s.warnings, ["privileges on public.t", "rules on public.\"a b\""]);
        assert_eq!(object_script("t", Ddl::complete("SELECT 1;\n".into())).text, "SELECT 1;\n");
    }

    #[test]
    fn privileges_list_each_change_with_its_column_and_grantee() {
        let grant = |privilege: &str, column: Option<&str>, grantee: &str, change: Change| Privilege {
            name: privilege.into(),
            column: column.map(Into::into),
            grantee: grantee.into(),
            change,
        };
        assert_eq!(
            privileges(
                "[dbo].[t]",
                &[
                    grant("SELECT", None, "[app]", Change::Granted),
                    grant("UPDATE", Some("[qty]"), "[app]", Change::GrantedWithGrantOption),
                    grant("EXECUTE", None, "PUBLIC", Change::Revoked),
                    grant("DELETE", None, "[app]", Change::Denied),
                ]
            )
            .as_deref(),
            Some(
                "privileges on [dbo].[t] (SELECT granted to [app], UPDATE ([qty]) granted with grant option to [app], EXECUTE revoked from PUBLIC, DELETE denied to [app])"
            )
        );
        assert_eq!(privileges("[dbo].[t]", &[]), None);
    }
}
