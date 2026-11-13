use super::super::error;
use crate::model::{ColumnMeta, QueryError, SqlTypeRef, TypeCategory};
use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use tiberius::{Column, ColumnData, ColumnType, FromSql};

pub(super) fn metadata(c: &Column) -> ColumnMeta {
    use ColumnType::*;
    let (display, category) = match c.column_type() {
        Money => ("money", TypeCategory::Numeric),
        Money4 => ("smallmoney", TypeCategory::Numeric),
        SSVariant => ("sql_variant", TypeCategory::Other),
        Udt => ("clr", TypeCategory::Binary),
        Int1 => ("tinyint", TypeCategory::Integer),
        Int2 => ("smallint", TypeCategory::Integer),
        Int4 => ("int", TypeCategory::Integer),
        Int8 => ("bigint", TypeCategory::Integer),
        Intn => ("integer", TypeCategory::Integer),
        Bit | Bitn => ("bit", TypeCategory::Bool),
        Float4 => ("real", TypeCategory::Numeric),
        Float8 | Floatn => ("float", TypeCategory::Numeric),
        Decimaln | Numericn => ("decimal", TypeCategory::Numeric),
        Datetime4 => ("smalldatetime", TypeCategory::Temporal),
        Datetime | Datetimen => ("datetime", TypeCategory::Temporal),
        Daten => ("date", TypeCategory::Temporal),
        Timen => ("time", TypeCategory::Temporal),
        Datetime2 => ("datetime2", TypeCategory::Temporal),
        DatetimeOffsetn => ("datetimeoffset", TypeCategory::Temporal),
        Guid => ("uniqueidentifier", TypeCategory::Uuid),
        BigVarBin | BigBinary | Image => ("varbinary", TypeCategory::Binary),
        Xml => ("xml", TypeCategory::Text),
        BigVarChar | BigChar | Text => ("varchar", TypeCategory::Text),
        NVarchar | NChar | NText => ("nvarchar", TypeCategory::Text),
        Null => ("null", TypeCategory::Other),
    };
    ColumnMeta { name: c.name().to_owned(), r#type: SqlTypeRef { display: display.into(), category }, source: None }
}

pub(super) fn decimal(value: i128, scale: u8) -> String {
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { "-" } else { "" };
    let scale = usize::from(scale);
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    if digits.len() <= scale {
        return format!("{sign}0.{}{}", "0".repeat(scale - digits.len()), digits);
    }
    let at = digits.len() - scale;
    format!("{sign}{}.{}", &digits[..at], &digits[at..])
}

pub(super) fn text(data: &ColumnData<'static>) -> Result<Option<String>, QueryError> {
    use ColumnData::*;
    let result = match data {
        U8(v) => v.map(|v| v.to_string()),
        I16(v) => v.map(|v| v.to_string()),
        I32(v) => v.map(|v| v.to_string()),
        I64(v) => v.map(|v| v.to_string()),
        F32(v) => v.map(|v| v.to_string()),
        F64(v) => v.map(|v| v.to_string()),
        Bit(v) => v.map(|v| if v { "1".into() } else { "0".into() }),
        String(v) => v.as_ref().map(|v| v.to_string()),
        Guid(v) => v.map(|v| v.to_string()),
        Binary(v) => v.as_ref().map(|v| {
            use std::fmt::Write;
            let mut s = std::string::String::from("0x");
            for b in v.iter() {
                write!(s, "{b:02X}").unwrap();
            }
            s
        }),
        Numeric(v) => v.map(|v| decimal(v.value(), v.scale())),
        Xml(v) => v.as_ref().map(|v| v.to_string()),
        Date(_) => NaiveDate::from_sql(data).map_err(super::query_error)?.map(|v| v.format("%Y-%m-%d").to_string()),
        Time(_) => NaiveTime::from_sql(data)
            .map_err(super::query_error)?
            .map(|v| format!("{}.{:07}", v.format("%H:%M:%S"), v.nanosecond() / 100)),
        DateTime2(_) => NaiveDateTime::from_sql(data)
            .map_err(super::query_error)?
            .map(|v| format!("{}.{:07}", v.format("%Y-%m-%d %H:%M:%S"), v.nanosecond() / 100)),
        SmallDateTime(_) => NaiveDateTime::from_sql(data)
            .map_err(super::query_error)?
            .map(|v| v.format("%Y-%m-%d %H:%M:%S").to_string()),
        DateTime(v) => match v {
            None => None,
            Some(v) => {
                let date = NaiveDate::from_ymd_opt(1900, 1, 1)
                    .unwrap()
                    .checked_add_signed(chrono::Duration::days(i64::from(v.days())))
                    .ok_or_else(|| error("Invalid datetime day"))?;
                let millis = (i64::from(v.seconds_fragments()) * 1000 + 150) / 300;
                let dt = date
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .checked_add_signed(chrono::Duration::milliseconds(millis))
                    .ok_or_else(|| error("Invalid datetime"))?;
                Some(dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string())
            }
        },
        DateTimeOffset(_) => chrono::DateTime::<FixedOffset>::from_sql(data).map_err(super::query_error)?.map(
            |v: chrono::DateTime<FixedOffset>| {
                format!("{}.{:07}{}", v.format("%Y-%m-%d %H:%M:%S"), v.nanosecond() / 100, v.format("%:z"))
            },
        ),
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decimals_keep_sign_leading_zero_scale_and_all_38_digits() {
        assert_eq!(decimal(-1, 38), "-0.00000000000000000000000000000000000001");
        assert_eq!(decimal(12345678901234567890123456789012345678, 12), "12345678901234567890123456.789012345678");
        assert_eq!(decimal(0, 12), "0.000000000000");
        assert_eq!(decimal(-1200, 2), "-12.00");
    }
}
