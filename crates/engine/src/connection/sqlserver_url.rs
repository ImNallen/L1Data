use crate::model::{ConnectionDraft, DatabaseKind, PasswordInput, SqlServerAuth, SslMode};
use percent_encoding::percent_decode_str;

fn decode(value: &str) -> Result<String, String> {
    let decoded =
        percent_decode_str(value).decode_utf8().map_err(|_| "SQL Server URL contains invalid UTF-8".to_owned())?;
    if decoded.chars().any(char::is_control) {
        return Err("SQL Server URL values cannot contain control characters".into());
    }
    Ok(decoded.into_owned())
}

fn authentication(value: &str) -> Result<SqlServerAuth, String> {
    if value.eq_ignore_ascii_case("ActiveDirectoryAzCli") {
        Ok(SqlServerAuth::EntraAzureCli)
    } else if value.eq_ignore_ascii_case("SqlPassword") {
        Ok(SqlServerAuth::SqlLogin)
    } else {
        Err("SQL Server Authentication must be SqlPassword or ActiveDirectoryAzCli".into())
    }
}

fn flag(key: &str, value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("SQL Server {key} must be true or false")),
    }
}

fn first_property(text: &str) -> Option<usize> {
    let names_a_value =
        |after: &str| after.split(';').next().and_then(|segment| segment.find('=')).is_some_and(|eq| eq > 0);
    text.match_indices(';').map(|(i, _)| i).find(|&i| names_a_value(&text[i + 1..]))
}

fn split_jdbc_properties(input: &str) -> Result<(&str, Option<&str>), String> {
    let Some(authority_start) = input.find("://").map(|i| i + 3) else { return Ok((input, None)) };
    let authority_end = input[authority_start..].find(['/', '?', '#']).map_or(input.len(), |i| authority_start + i);
    let start = match first_property(&input[authority_start..authority_end]) {
        Some(i) => {
            let start = authority_start + i;
            if input[start + 1..]
                .split(';')
                .any(|p| p.contains('@') && !p.split_once('=').is_some_and(|(key, _)| key.eq_ignore_ascii_case("user")))
            {
                return Err("This SQL Server URL is ambiguous. An @ after its first ;name=value property could also end the user name and password. Percent-encode a ; in a password as %3B, or an @ in a property value as %40. Only a user value can hold a plain @.".into());
            }
            start
        }
        None if input[authority_end..].starts_with('/') => {
            let path_end = input[authority_end..].find(['?', '#']).map_or(input.len(), |i| authority_end + i);
            let Some(i) = first_property(&input[authority_end..path_end]) else { return Ok((input, None)) };
            authority_end + i
        }
        None => return Ok((input, None)),
    };
    if input[..start].ends_with(';') {
        return Err("SQL Server URL properties need a name and value".into());
    }
    let properties = &input[start + 1..];
    if let Some((c, encoded)) = properties.chars().find_map(|c| match c {
        '/' => Some((c, "%2F")),
        '?' => Some((c, "%3F")),
        '#' => Some((c, "%23")),
        _ => None,
    }) {
        return Err(format!(
            "SQL Server URL properties run to the end of the URL. Percent-encode a {c} inside a property value as {encoded}."
        ));
    }
    Ok((&input[..start], Some(properties)))
}

pub(super) fn parse(input: &str, name: &str) -> Result<ConnectionDraft, String> {
    if input.chars().any(char::is_control) {
        return Err("SQL Server URL cannot contain control characters".into());
    }
    for (i, byte) in input.bytes().enumerate() {
        if byte == b'%'
            && !input.as_bytes().get(i + 1..i + 3).is_some_and(|pair| pair.iter().all(u8::is_ascii_hexdigit))
        {
            return Err("SQL Server URL contains invalid percent encoding".into());
        }
    }
    let (input, properties) = split_jdbc_properties(input)?;
    let (_, authority) = input.split_once("://").ok_or("Use mssql:// or sqlserver:// with a TCP host")?;
    let raw_path =
        authority.split(['?', '#']).next().unwrap_or_default().split_once('/').map(|(_, p)| p).unwrap_or_default();
    if raw_path.contains('/') || matches!(raw_path, "." | "..") {
        return Err("SQL Server URL must contain a single database name; percent-encode slashes in that name".into());
    }
    let url = url::Url::parse(input).map_err(|e| format!("Invalid SQL Server URL: {e}"))?;
    if url.fragment().is_some() {
        return Err("SQL Server URL fragments are unsupported".into());
    }
    let host = url.host_str().filter(|h| !h.is_empty()).ok_or("SQL Server URL needs a TCP host")?;
    if host.contains(';') {
        return Err("SQL Server URL properties need a name and value".into());
    }
    if host.contains([',', '\\', '%']) {
        return Err("SQL Server URL requires one TCP host; named instances and multi-host URLs are unsupported".into());
    }
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host).to_owned();
    let port = url.port().unwrap_or(1433);
    if port == 0 {
        return Err("SQL Server port must be between 1 and 65535".into());
    }
    let mut ssl_mode = None;
    let mut sql_server_auth = None;
    if let Some(query) = url.query().filter(|q| !q.is_empty()) {
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').ok_or("SQL Server URL options need a name and value")?;
            let key = decode(&key.replace('+', " "))?;
            let value = decode(&value.replace('+', " "))?;
            if key == "sslmode" {
                let mode = match value.as_str() {
                    "require" => SslMode::Require,
                    "verify-full" => SslMode::VerifyFull,
                    _ => return Err("SQL Server sslmode must be require or verify-full".into()),
                };
                if ssl_mode.replace(mode).is_some() {
                    return Err("SQL Server URL repeats sslmode".into());
                }
            } else if key.eq_ignore_ascii_case("authentication") {
                if sql_server_auth.replace(authentication(&value)?).is_some() {
                    return Err("SQL Server URL repeats Authentication".into());
                }
            } else {
                return Err(format!("Unsupported SQL Server URL option: {key}"));
            }
        }
    }
    let mut database = (!raw_path.is_empty()).then(|| decode(raw_path)).transpose()?;
    let mut user = decode(url.username())?;
    let mut password = url.password().map(decode).transpose()?;
    let mut encrypt = None;
    for property in properties.into_iter().flat_map(|p| p.split(';')).filter(|p| !p.is_empty()) {
        let (key, value) = property.split_once('=').ok_or("SQL Server URL properties need a name and value")?;
        let value = decode(value)?;
        let repeated = match key.to_ascii_lowercase().as_str() {
            "database" | "databasename" => database.replace(value).is_some(),
            "user" => !std::mem::replace(&mut user, value).is_empty(),
            "password" => password.replace(value).is_some(),
            "authentication" => sql_server_auth.replace(authentication(&value)?).is_some(),
            "encrypt" if value.eq_ignore_ascii_case("mandatory") || flag(key, &value)? => encrypt.replace(()).is_some(),
            "encrypt" => return Err("SQL Server connections are always encrypted, so encrypt must be true".into()),
            "trustservercertificate" => {
                ssl_mode.replace(if flag(key, &value)? { SslMode::Require } else { SslMode::VerifyFull }).is_some()
            }
            _ => return Err(format!("Unsupported SQL Server URL property: {key}")),
        };
        if repeated {
            return Err(format!("SQL Server URL repeats {key}"));
        }
    }
    if sql_server_auth == Some(SqlServerAuth::EntraAzureCli) {
        password = None;
    }
    Ok(ConnectionDraft {
        id: None,
        database_kind: DatabaseKind::SqlServer,
        name: name.into(),
        host,
        port,
        database: database.unwrap_or_else(|| "master".into()),
        user,
        password: password.map_or(PasswordInput::Clear, PasswordInput::Set),
        ssl_mode: ssl_mode.unwrap_or(SslMode::VerifyFull),
        ca_cert_path: None,
        color: None,
        ssh: None,
        environment: None,
        sql_server_auth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::draft_from_url;

    #[test]
    fn both_schemes_default_to_encrypted_sql_server_and_decode_unicode_values() {
        for scheme in ["mssql", "sqlserver", "SQLSERVER"] {
            let draft = draft_from_url(&format!("{scheme}://localhost"), "local").unwrap();
            assert_eq!(
                (draft.database_kind, draft.port, draft.ssl_mode),
                (DatabaseKind::SqlServer, 1433, SslMode::VerifyFull)
            );
            assert_eq!(draft.database, "master");
            assert_eq!(draft.name, "local");
        }
        let draft = draft_from_url(
            "sqlserver://a%40b:p%3A%2F%40%F0%9F%98%80@localhost:1434/shop%20%2F%20%E4%B8%96%E7%95%8C?sslmode=require",
            "test",
        )
        .unwrap();
        assert_eq!(
            (draft.host.as_str(), draft.port, draft.database.as_str(), draft.user.as_str()),
            ("localhost", 1434, "shop / 世界", "a@b")
        );
        assert!(matches!(draft.password, PasswordInput::Set(p) if p == "p:/@😀"));
        assert_eq!(draft.ssl_mode, SslMode::Require);
        assert_eq!(
            draft_from_url("mssql://sa@localhost/shop?sslmode=verify-full", "").unwrap().ssl_mode,
            SslMode::VerifyFull
        );
        assert_eq!(draft_from_url("mssql://[::1]:1434/shop", "").unwrap().host, "::1");
    }

    #[test]
    fn authentication_selects_entra_from_a_query_or_jdbc_properties() {
        let draft =
            draft_from_url("sqlserver://db.example.net;database=shop;Authentication=ActiveDirectoryAzCli", "").unwrap();
        assert_eq!((draft.host.as_str(), draft.port, draft.database.as_str()), ("db.example.net", 1433, "shop"));
        assert_eq!(draft.sql_server_auth, Some(SqlServerAuth::EntraAzureCli));
        assert!(matches!(draft.password, PasswordInput::Clear));
        let draft = draft_from_url(
            "mssql://me:secret@db.example.net:1444/shop?sslmode=require&authentication=activedirectoryazcli",
            "",
        )
        .unwrap();
        assert_eq!(
            (draft.port, draft.ssl_mode, draft.sql_server_auth),
            (1444, SslMode::Require, Some(SqlServerAuth::EntraAzureCli))
        );
        assert!(matches!(draft.password, PasswordInput::Clear), "an Entra draft carries no password");
        let draft = draft_from_url(
            "sqlserver://localhost:1434;databaseName=shop%3Bx;user=sa;password=p%3Bw;Authentication=SqlPassword",
            "",
        )
        .unwrap();
        assert_eq!(
            (draft.database.as_str(), draft.user.as_str(), draft.sql_server_auth),
            ("shop;x", "sa", Some(SqlServerAuth::SqlLogin))
        );
        assert!(matches!(draft.password, PasswordInput::Set(p) if p == "p;w"));
        assert_eq!(draft_from_url("sqlserver://localhost/shop", "").unwrap().sql_server_auth, None);
    }

    #[test]
    fn a_semicolon_in_the_user_info_or_the_path_reads_as_part_of_it() {
        let password = |url: &str| match draft_from_url(url, "").unwrap().password {
            PasswordInput::Set(p) => p,
            other => panic!("{url}: {other:?}"),
        };
        let draft = draft_from_url("sqlserver://sa:pa;ss@localhost:1434/shop", "").unwrap();
        assert_eq!(
            (draft.user.as_str(), draft.host.as_str(), draft.port, draft.database.as_str()),
            ("sa", "localhost", 1434, "shop")
        );
        assert_eq!(password("sqlserver://sa:pa;ss@localhost:1434/shop"), "pa;ss");
        assert_eq!(password("sqlserver://sa:a%3Bb=c@localhost/shop"), "a;b=c");
        assert_eq!(draft_from_url("sqlserver://localhost/my;db", "").unwrap().database, "my;db");
        assert_eq!(password("sqlserver://localhost;user=sa;password=a%2Fb%40c"), "a/b@c");
    }

    #[test]
    fn an_empty_property_before_the_first_one_is_refused_after_the_host_or_the_path() {
        for url in [
            "sqlserver://host;",
            "sqlserver://host;;database=x",
            "sqlserver://host/db;;encrypt=true",
            "sqlserver://sa:pw@host:1433/db;;authentication=ActiveDirectoryAzCli",
        ] {
            assert_eq!(
                draft_from_url(url, "").unwrap_err(),
                "SQL Server URL properties need a name and value",
                "{url}"
            );
        }
    }

    #[test]
    fn properties_after_the_path_read_as_properties() {
        let draft = draft_from_url("sqlserver://db.example.net/shop;authentication=ActiveDirectoryAzCli", "").unwrap();
        assert_eq!(
            (draft.host.as_str(), draft.database.as_str(), draft.sql_server_auth),
            ("db.example.net", "shop", Some(SqlServerAuth::EntraAzureCli))
        );
        let draft = draft_from_url(
            "sqlserver://sa:pa;ss@db.example.net:1433/shop;encrypt=true;trustServerCertificate=true",
            "",
        )
        .unwrap();
        assert_eq!(
            (draft.host.as_str(), draft.port, draft.database.as_str(), draft.user.as_str(), draft.ssl_mode),
            ("db.example.net", 1433, "shop", "sa", SslMode::Require)
        );
        assert!(matches!(draft.password, PasswordInput::Set(p) if p == "pa;ss"));
        let draft = draft_from_url("sqlserver://db.example.net/shop;user=me@contoso.com;password=p@ss", "").unwrap();
        assert_eq!((draft.host.as_str(), draft.user.as_str()), ("db.example.net", "me@contoso.com"));
        assert!(
            matches!(draft.password, PasswordInput::Set(p) if p == "p@ss"),
            "after the path an @ cannot end the user info"
        );
        for (url, refused) in [
            ("mssql://localhost/shop;database=other", "repeats database"),
            ("sqlserver://host:1433/db;encrypt=false", "always encrypted"),
            ("sqlserver://host/db;applicationName=x", "Unsupported SQL Server URL property"),
            ("sqlserver://host/db;password=a/b", "a / inside a property value as %2F"),
            ("sqlserver://host/db;encrypt=true?sslmode=require", "a ? inside a property value as %3F"),
            ("sqlserver://host/a/b;encrypt=true", "single database name"),
        ] {
            let message = draft_from_url(url, "").unwrap_err();
            assert!(message.contains(refused), "{url}: {message}");
        }
    }

    #[test]
    fn an_at_sign_after_the_first_property_outside_a_user_value_is_ambiguous() {
        for url in [
            "sqlserver://admin:12;password=hunter2@db.corp",
            "sqlserver://srv;app-name=foo@evil.com;password=pw",
            "sqlserver://srv;a:b=c@evil.com;password=pw",
            "sqlserver://srv;a@b=c@evil.com;password=pw",
            "sqlserver://sa:a;b=c@localhost/shop",
            "sqlserver://sa:123;database=evil@localhost/shop",
            "sqlserver://sa@db.example.net:1444;password=p@ss@word",
            "sqlserver://db.example.net;Initial Catalog=a@b",
            "sqlserver://db.example.net;database=shop;x@y",
        ] {
            let message = draft_from_url(url, "").unwrap_err();
            assert!(
                message.contains("ambiguous")
                    && message.contains("; in a password as %3B")
                    && message.contains("@ in a property value as %40"),
                "{url}: {message}"
            );
            assert!(!message.contains("%2F"), "{url} names only the characters that apply: {message}");
        }
        let draft = draft_from_url("sqlserver://sa@db.example.net:1444;password=p%40ss%40word", "").unwrap();
        assert_eq!((draft.host.as_str(), draft.port, draft.user.as_str()), ("db.example.net", 1444, "sa"));
        assert!(matches!(draft.password, PasswordInput::Set(p) if p == "p@ss@word"));
    }

    #[test]
    fn an_at_sign_in_a_user_value_never_ends_the_user_info() {
        let draft =
            draft_from_url("sqlserver://db.example.net;user=me@contoso.com;authentication=ActiveDirectoryAzCli", "")
                .unwrap();
        assert_eq!(
            (draft.host.as_str(), draft.sql_server_auth),
            ("db.example.net", Some(SqlServerAuth::EntraAzureCli))
        );
        let draft = draft_from_url(
            "sqlserver://srv.database.windows.net:1433;database=mydb;user=myadmin@myserver;password=pw;encrypt=true",
            "",
        )
        .unwrap();
        assert_eq!(
            (draft.host.as_str(), draft.port, draft.database.as_str(), draft.user.as_str()),
            ("srv.database.windows.net", 1433, "mydb", "myadmin@myserver")
        );
        assert!(matches!(draft.password, PasswordInput::Set(p) if p == "pw"));
    }

    #[test]
    fn a_path_query_or_fragment_after_properties_is_refused_naming_the_character() {
        for (url, named) in [
            ("sqlserver://host;database=shop?sslmode=require", "a ? inside a property value as %3F"),
            ("sqlserver://host;database=shop#frag", "a # inside a property value as %23"),
            ("sqlserver://host;database=shop/x", "a / inside a property value as %2F"),
            ("sqlserver://localhost;user=sa;password=a/b", "a / inside a property value as %2F"),
        ] {
            let message = draft_from_url(url, "").unwrap_err();
            assert!(message.contains(named) && message.matches('%').count() == 1, "{url}: {message}");
        }
        let draft = draft_from_url("sqlserver://host;database=shop%3Fx%23y%2Fz", "").unwrap();
        assert_eq!(draft.database, "shop?x#y/z");
    }

    #[test]
    fn azure_portal_encrypt_and_trust_properties_map_to_ssl_modes() {
        let mode = |url: &str| draft_from_url(url, "").unwrap().ssl_mode;
        assert_eq!(
            mode(
                "sqlserver://db.example.net:1433;database=shop;user=me;password=pw;encrypt=true;trustServerCertificate=false"
            ),
            SslMode::VerifyFull
        );
        assert_eq!(mode("sqlserver://db.example.net;Encrypt=True;TrustServerCertificate=True"), SslMode::Require);
        assert_eq!(mode("sqlserver://db.example.net;encrypt=mandatory"), SslMode::VerifyFull);
        for url in [
            "sqlserver://db.example.net;encrypt=false",
            "sqlserver://db.example.net;encrypt=strict",
            "sqlserver://db.example.net;trustServerCertificate=maybe",
            "sqlserver://db.example.net;encrypt=true;encrypt=true",
            "sqlserver://db.example.net;trustServerCertificate=true;trustServerCertificate=false",
        ] {
            assert!(draft_from_url(url, "").is_err(), "{url}");
        }
    }

    #[test]
    fn unsupported_or_unrepresentable_options_are_rejected() {
        for url in [
            "mssql://localhost/shop?sslmode=disable",
            "mssql://localhost/shop?sslmode=verify-ca",
            "mssql://localhost/shop?sslrootcert=ca.pem",
            "mssql://localhost/shop?encrypt=false",
            "mssql://localhost/shop?integratedSecurity=true",
            "mssql://localhost/shop?applicationName=test",
            "mssql://localhost/shop?sslmode=require&sslmode=verify-full",
            "mssql://localhost/shop#ignored",
            "mssql://localhost/a/b",
            "mssql://localhost/a/../shop",
            "mssql://localhost:0/shop",
            "mssql://sa:%FF@localhost/shop",
            "mssql://sa:bad%2@localhost/shop",
            "mssql://localhost/shop%00",
            "mssql://localhost,other/shop",
            "mssql://localhost/shop?sslmode",
            "mssql:shop",
            "mssql://localhost/shop?Authentication=ActiveDirectoryPassword",
            "mssql://localhost/shop?authentication=ActiveDirectoryAzCli&Authentication=SqlPassword",
            "mssql://localhost;database=a;databaseName=b",
            "mssql://sa@localhost;user=other",
            "mssql://localhost;encrypt=false",
            "mssql://localhost;database",
        ] {
            assert!(draft_from_url(url, "").is_err(), "{url}");
        }
    }
}
