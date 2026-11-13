use super::tsql::quote_literal;
use crate::model::ColumnInfo;

fn base_type(native: &str) -> &str {
    native.split('(').next().unwrap_or(native)
}

/// Date and time convert with an explicit style, so DATEFORMAT cannot change them.
pub(crate) fn input_sql(native: &str, value: &str) -> Result<String, String> {
    let expression = quote_literal(value);
    let base = base_type(native);
    let bound = native.strip_suffix(')').and_then(|s| s.split_once('(')).and_then(|(_, n)| n.parse::<usize>().ok());
    match base {
        "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" => {
            if matches!(base, "nchar" | "nvarchar") && bound.is_some_and(|n| value.encode_utf16().count() > n) {
                return Err(format!("Text value exceeds the capacity of {native}"));
            }
            Ok(expression)
        }
        "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => {
            let hex = value.strip_prefix("0x").ok_or("Binary values must use 0x-prefixed hexadecimal text")?;
            if hex.len() % 2 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Binary values must contain complete hexadecimal bytes".into());
            }
            let bound = if matches!(base, "timestamp" | "rowversion") { Some(8) } else { bound };
            if bound.is_some_and(|n| hex.len() / 2 > n) {
                return Err(format!("Binary value exceeds the capacity of {native}"));
            }
            Ok(format!("CONVERT(varbinary(max), {expression}, 1)"))
        }
        "date" => Ok(format!("CONVERT({native}, {expression}, 23)")),
        "time" | "datetime" | "smalldatetime" | "datetime2" | "datetimeoffset" => {
            Ok(format!("CONVERT({native}, {expression}, 121)"))
        }
        "tinyint" | "smallint" | "int" | "bigint" | "decimal" | "numeric" | "money" | "smallmoney" | "float"
        | "real" | "bit" | "uniqueidentifier" | "xml" => Ok(format!("CONVERT({native}, {expression})")),
        _ => Err(format!("Canonical input for SQL Server type {native} is unsupported")),
    }
}

pub(crate) fn comparison_sql(column: &ColumnInfo, expression: &str, value: &str) -> Result<(String, String), String> {
    let input = || input_sql(&column.type_display, value).map_err(|e| format!("{}: {e}", column.name));
    match base_type(&column.type_display) {
        "text" | "ntext" | "xml" => Ok((format!("CONVERT(nvarchar(max), {expression})"), quote_literal(value))),
        "image" => Ok((format!("CONVERT(varbinary(max), {expression})"), input()?)),
        _ => Ok((expression.into(), input()?)),
    }
}

pub(crate) fn display_sql(column: &ColumnInfo, expression: &str) -> Result<String, String> {
    let display = match base_type(&column.type_display) {
        "binary" | "varbinary" | "image" | "timestamp" | "rowversion" => {
            format!("CONVERT(varchar(max), CONVERT(varbinary(max), {expression}), 1)")
        }
        "date" => format!("CONVERT(nvarchar(max), {expression}, 23)"),
        "smalldatetime" => format!("CONVERT(nvarchar(max), {expression}, 120)"),
        "datetime" => format!("CONVERT(nvarchar(max), {expression}, 121)"),
        "time" => format!("CONVERT(nvarchar(max), CONVERT(time(7), {expression}), 121)"),
        "datetime2" => format!("CONVERT(nvarchar(max), CONVERT(datetime2(7), {expression}), 121)"),
        "datetimeoffset" => format!(
            "CONVERT(nvarchar(max), CONVERT(datetime2(7), {expression}), 121) + DATENAME(TZOFFSET, {expression})"
        ),
        "money" | "smallmoney" => format!("CONVERT(nvarchar(max), CONVERT(decimal(19,4), {expression}))"),
        "uniqueidentifier" => format!("LOWER(CONVERT(nvarchar(max), {expression}))"),
        "float" | "real" => {
            return Err(format!(
                "Text search cannot reproduce the canonical display of SQL Server {}. Use a typed comparison or an explicit SQL conversion.",
                column.type_display
            ));
        }
        "char" | "varchar" | "nchar" | "nvarchar" | "text" | "ntext" | "xml" | "tinyint" | "smallint" | "int"
        | "bigint" | "decimal" | "numeric" | "bit" => {
            format!("CONVERT(nvarchar(max), {expression})")
        }
        native => return Err(format!("Text search for SQL Server type {native} is unsupported")),
    };
    Ok(display)
}

#[cfg(all(test, feature = "mssql-tests"))]
mod tests {
    use crate::connection::ConnectParams;
    use crate::model::*;
    use crate::session::Session;
    use crate::text;
    use futures_util::FutureExt;
    use std::panic::AssertUnwindSafe;

    #[tokio::test]
    async fn generated_counts_use_canonical_values_under_each_session_dateformat() {
        let params = ConnectParams {
            database_kind: DatabaseKind::SqlServer,
            host: std::env::var("L1_DATA_TEST_MSSQL_HOST").unwrap_or_else(|_| "localhost".into()),
            port: std::env::var("L1_DATA_TEST_MSSQL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(1434),
            database: "shop".into(),
            user: "sa".into(),
            password: Some(std::env::var("L1_DATA_TEST_MSSQL_PASSWORD").unwrap_or_else(|_| "L1Data_dev_Str0ng".into())),
            ssl_mode: SslMode::Require,
            ca_cert_path: None,
            via: None,
            entra: None,
        };
        let mut session = Session::open(&params, None, "UTC").await.unwrap();
        let table = format!("l1_typed_count_{}", uuid::Uuid::new_v4().simple());
        let outcome = AssertUnwindSafe(async {
            session.batch(&format!("CREATE TABLE dbo.[{table}] (old datetime, small smalldatetime, day date, modern datetime2(7), moment datetimeoffset(7), tick time(7), bytes varbinary(4), uid uniqueidentifier); INSERT INTO dbo.[{table}] VALUES (N'2024-01-02T03:04:05.123',N'2024-01-02T03:04:00',N'20240102',N'2024-01-02T03:04:05.1234567',N'2024-01-02T03:04:05.1234567+02:00',N'03:04:05.1234567',0xDEADBEEF,N'abcd1234-5678-90ab-cdef-1234567890ab')")).await.unwrap();
            let db = DatabaseRef { connection_id: ConnectionId(uuid::Uuid::new_v4()), database: "shop".into() };
            let catalog = session.catalog(db, 1, &[]).await.unwrap();
            let relation = catalog.relation("dbo", &table).unwrap();
            for dateformat in ["dmy", "mdy"] {
                session.batch(&format!("SET DATEFORMAT {dateformat}")).await.unwrap();
                for (column, value) in [
                    ("old", "2024-01-02 03:04:05.123"),
                    ("small", "2024-01-02 03:04:00"),
                    ("day", "2024-01-02"),
                    ("modern", "2024-01-02 03:04:05.1234567"),
                    ("moment", "2024-01-02 03:04:05.1234567+02:00"),
                    ("tick", "03:04:05.1234567"),
                    ("bytes", "0xDEADBEEF"),
                    ("uid", "abcd1234-5678-90ab-cdef-1234567890ab"),
                ] {
                    for (op, expected) in [(FilterOp::Eq, 1), (FilterOp::Ne, 0), (FilterOp::Contains, 1), (FilterOp::StartsWith, 1)] {
                        let query = TableQuery { filters: vec![ColumnFilter { column: column.into(), op, value: value.into() }], ..Default::default() };
                        let sql = text::table_count(DatabaseKind::SqlServer, "dbo", &table, Some(relation), &query).unwrap();
                        assert_eq!(session.count(&sql).await.unwrap(), expected, "{column} {op:?} under {dateformat}");
                    }
                }
            }
            let uid = relation.columns.iter().find(|column| column.name == "uid").unwrap();
            let display = super::display_sql(uid, "[uid]").unwrap();
            for collation in ["Latin1_General_100_CS_AS", "Latin1_General_100_BIN2"] {
                let sql = format!("SELECT COUNT_BIG(*) FROM dbo.[{table}] WHERE {display} COLLATE {collation} LIKE N'abcd1234-5678-90ab-cdef-1234567890ab'");
                assert_eq!(session.count(&sql).await.unwrap(), 1, "UUID display under {collation}");
            }
        })
        .catch_unwind()
        .await;
        session.batch(&format!("DROP TABLE IF EXISTS dbo.[{table}]")).await.unwrap();
        assert_eq!(
            session.count(&format!("SELECT COUNT_BIG(*) FROM sys.tables WHERE name=N'{table}'")).await.unwrap(),
            0
        );
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
}
