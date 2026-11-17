//! The T-SQL built-in functions L1 Data vouches for when it runs a user's query again, and what each
//! one's result depends on.

use super::Token;

/// A built-in function a vetted query may call, by its bare name.
pub(crate) struct Builtin {
    pub(crate) name: &'static str,
    /// Whether the same arguments give the same result in another session at a later time. A clock or
    /// the session's user does not. `SET` options such as `LANGUAGE` and `DATEFIRST` are left out: the
    /// docs say a new cell's session does not carry them over.
    pub(crate) reproducible: bool,
}

const fn pure(name: &'static str) -> Builtin {
    Builtin { name, reproducible: true }
}

const fn varies(name: &'static str) -> Builtin {
    Builtin { name, reproducible: false }
}

/// Random functions such as `NEWID` and `RAND`, and session functions such as `USER_NAME` and
/// `APP_NAME`, are not here, so a query that calls them is not vetted at all.
pub(crate) const BUILTINS: &[Builtin] = &[
    pure("ABS"),
    pure("APPROX_COUNT_DISTINCT"),
    pure("ASCII"),
    pure("AVG"),
    pure("CAST"),
    pure("CEILING"),
    pure("CHAR"),
    pure("CHARINDEX"),
    pure("CHECKSUM_AGG"),
    pure("CHOOSE"),
    pure("COALESCE"),
    pure("CONCAT"),
    pure("CONCAT_WS"),
    pure("CONVERT"),
    pure("COUNT"),
    pure("COUNT_BIG"),
    pure("CUME_DIST"),
    varies("CURRENT_DATE"),
    varies("CURRENT_TIME"),
    varies("CURRENT_TIMESTAMP"),
    varies("CURRENT_USER"),
    pure("DATALENGTH"),
    pure("DATEADD"),
    pure("DATEDIFF"),
    pure("DATEDIFF_BIG"),
    pure("DATEFROMPARTS"),
    pure("DATENAME"),
    pure("DATEPART"),
    pure("DATETIME2FROMPARTS"),
    pure("DATETIMEFROMPARTS"),
    pure("DATETRUNC"),
    pure("DATE_BUCKET"),
    pure("DAY"),
    pure("DENSE_RANK"),
    pure("EOMONTH"),
    pure("FIRST_VALUE"),
    pure("FLOOR"),
    pure("FORMAT"),
    pure("GENERATE_SERIES"),
    varies("GETDATE"),
    varies("GETUTCDATE"),
    pure("GREATEST"),
    pure("IIF"),
    pure("ISDATE"),
    pure("ISJSON"),
    pure("ISNULL"),
    pure("ISNUMERIC"),
    pure("JSON_QUERY"),
    pure("JSON_VALUE"),
    pure("LAG"),
    pure("LAST_VALUE"),
    pure("LEAD"),
    pure("LEAST"),
    pure("LEFT"),
    pure("LEN"),
    pure("LOWER"),
    pure("LTRIM"),
    pure("MAX"),
    pure("MIN"),
    pure("MONTH"),
    pure("NCHAR"),
    pure("NTILE"),
    pure("NULLIF"),
    pure("OPENJSON"),
    pure("PATINDEX"),
    pure("PERCENT_RANK"),
    pure("POWER"),
    pure("QUOTENAME"),
    pure("RANK"),
    pure("REPLACE"),
    pure("REPLICATE"),
    pure("REVERSE"),
    pure("RIGHT"),
    pure("ROUND"),
    pure("ROW_NUMBER"),
    pure("RTRIM"),
    varies("SESSION_USER"),
    pure("SIGN"),
    pure("SPACE"),
    pure("SQRT"),
    pure("SQUARE"),
    pure("STDEV"),
    pure("STDEVP"),
    pure("STR"),
    pure("STRING_AGG"),
    pure("STRING_SPLIT"),
    pure("STUFF"),
    pure("SUBSTRING"),
    pure("SUM"),
    varies("SYSDATETIME"),
    varies("SYSDATETIMEOFFSET"),
    varies("SYSTEM_USER"),
    varies("SYSUTCDATETIME"),
    pure("TIMEFROMPARTS"),
    pure("TRANSLATE"),
    pure("TRIM"),
    pure("TRY_CAST"),
    pure("TRY_CONVERT"),
    pure("UNICODE"),
    pure("UPPER"),
    varies("USER"),
    pure("VAR"),
    pure("VARP"),
    pure("YEAR"),
];

/// The built-in a word names, in any case.
pub(super) fn builtin(token: &Token<'_>) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| token.word(b.name))
}
