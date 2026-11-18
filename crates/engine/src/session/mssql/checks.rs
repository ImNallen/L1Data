/// Whose expression dependencies count, besides the table's own computed columns.
#[derive(Clone, Copy)]
pub(super) enum Children {
    /// Every child object, triggers included, as CSV import counts them.
    All,
    /// `CHECK` and default constraints. A staged edit leaves out disabled triggers' bodies, because a disabled
    /// trigger cannot run and enabling one needs the schema-modification lock the edit's lock blocks.
    Constraints,
}

/// A `CROSS APPLY` body that returns one `(reason, detail)` row for each way the table whose `object_id` is
/// `table` could run server code during a write, or hide that code from this check.
pub(super) fn violations(table: &str, children: Children) -> String {
    let children = match children {
        Children::All => format!("SELECT o.object_id FROM sys.objects o WHERE o.parent_object_id = {table}"),
        Children::Constraints => {
            format!("SELECT o.object_id FROM sys.objects o WHERE o.parent_object_id = {table} AND o.type IN ('C', 'D')")
        }
    };
    format!(
        r#"SELECT N'kind', CASE WHEN t.is_memory_optimized = 1 THEN N'memory-optimized' WHEN t.temporal_type <> 0 THEN N'temporal'
        WHEN t.is_node = 1 OR t.is_edge = 1 THEN N'graph' WHEN t.ledger_type <> 0 THEN N'ledger' WHEN t.is_filetable = 1 THEN N'FileTable'
        WHEN t.is_remote_data_archive_enabled = 1 THEN N'Stretch' ELSE N'replicated' END
    FROM sys.tables t WHERE t.object_id = {table} AND (t.is_memory_optimized = 1 OR t.temporal_type <> 0 OR t.is_node = 1 OR t.is_edge = 1
        OR t.is_filetable = 1 OR t.is_replicated = 1 OR t.is_merge_published = 1 OR t.is_sync_tran_subscribed = 1
        OR t.is_remote_data_archive_enabled = 1 OR t.ledger_type <> 0)
    UNION ALL SELECT N'kind', N'external' FROM sys.external_tables e WHERE e.object_id = {table}
    UNION ALL SELECT N'trigger', OBJECT_SCHEMA_NAME(tr.object_id) + N'.' + tr.name FROM sys.triggers tr WHERE tr.parent_id = {table} AND tr.is_disabled = 0
    UNION ALL SELECT N'policy', OBJECT_SCHEMA_NAME(sp.object_id) + N'.' + OBJECT_NAME(sp.object_id)
        FROM sys.security_predicates sp WHERE sp.target_object_id = {table}
    UNION ALL SELECT N'ignore_dup_key', i.name FROM sys.indexes i WHERE i.object_id = {table} AND i.ignore_dup_key = 1
    UNION ALL SELECT N'indexed_view', OBJECT_SCHEMA_NAME(v.object_id) + N'.' + v.name
        FROM sys.sql_expression_dependencies d JOIN sys.views v ON v.object_id = d.referencing_id
        JOIN sys.indexes i ON i.object_id = v.object_id AND i.index_id = 1 WHERE d.referenced_id = {table}
    UNION ALL SELECT CASE WHEN ty.is_assembly_type = 1 OR c.system_type_id = 240 THEN N'clr' WHEN ty.is_user_defined = 1 THEN N'alias'
            WHEN c.encryption_type IS NOT NULL THEN N'encrypted' WHEN c.rule_object_id <> 0 OR ty.rule_object_id <> 0 THEN N'bound_rule'
            ELSE N'bound_default' END, c.name
        FROM sys.columns c JOIN sys.types ty ON ty.user_type_id = c.user_type_id
        WHERE c.object_id = {table} AND (ty.is_assembly_type = 1 OR c.system_type_id = 240 OR ty.is_user_defined = 1 OR c.encryption_type IS NOT NULL
            OR c.rule_object_id <> 0 OR ty.rule_object_id <> 0 OR ty.default_object_id <> 0
            OR (c.default_object_id <> 0 AND NOT EXISTS (SELECT 1 FROM sys.default_constraints dc WHERE dc.object_id = c.default_object_id)))
    UNION ALL SELECT N'function', COALESCE(d.referenced_schema_name + N'.', N'') + d.referenced_entity_name
        FROM sys.sql_expression_dependencies d
        WHERE (d.referencing_id = {table} OR d.referencing_id IN ({children}))
            AND (d.is_ambiguous = 1 OR d.is_caller_dependent = 1 OR d.referenced_server_name IS NOT NULL
                OR d.referenced_database_name IS NOT NULL OR d.referenced_id IS NULL
                OR (d.referenced_id <> {table} AND NOT EXISTS (SELECT 1 FROM sys.sequences s WHERE s.object_id = d.referenced_id)))
    UNION ALL SELECT N'permission', N'VIEW DEFINITION'
        WHERE ISNULL(HAS_PERMS_BY_NAME(QUOTENAME(OBJECT_SCHEMA_NAME({table})) + N'.' + QUOTENAME(OBJECT_NAME({table})), 'OBJECT', 'VIEW DEFINITION'), 0) <> 1"#
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_fragment_names_every_table_kind_the_container_cannot_create() {
        let sql = super::violations("@id", super::Children::Constraints);
        for kind in [
            "is_memory_optimized = 1",
            "is_filetable = 1",
            "is_remote_data_archive_enabled = 1",
            "sys.external_tables",
            "is_replicated = 1",
            "is_merge_published = 1",
            "is_sync_tran_subscribed = 1",
        ] {
            assert!(sql.contains(kind), "{kind}");
        }
        assert!(sql.contains("o.type IN ('C', 'D')"));
        assert!(!super::violations("@id", super::Children::All).contains("o.type IN"));
    }
}
