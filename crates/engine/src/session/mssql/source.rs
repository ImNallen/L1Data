use super::catalog::display;
use super::{MsSession, native_object_id, number, required};
use crate::model::*;
use crate::script::{self, Change, Privilege, tsql as ddl};
use crate::sql::tsql::{quote_ident, quote_literal};

fn failed(e: QueryError) -> DetailError {
    DetailError::Failed { message: e.message }
}

fn refused(message: impl Into<String>) -> DetailError {
    DetailError::Failed { message: message.into() }
}

fn bit(row: &[Option<String>], i: usize) -> Result<bool, DetailError> {
    Ok(number::<u8>(row, i).map_err(failed)? != 0)
}

fn text(row: &[Option<String>], i: usize) -> Result<String, DetailError> {
    Ok(required(row, i).map_err(failed)?.to_owned())
}

const FEATURES: &str = "SELECT COL_LENGTH('sys.indexes','optimize_for_sequential_key'),COL_LENGTH('sys.index_columns','column_store_order_ordinal'),
    COL_LENGTH('sys.partitions','xml_compression'),COL_LENGTH('sys.tables','ledger_type'),COL_LENGTH('sys.tables','is_node'),COL_LENGTH('sys.tables','history_retention_period'),
    COL_LENGTH('sys.indexes','suppress_dup_key_messages'),COL_LENGTH('sys.sensitivity_classifications','label'),COL_LENGTH('sys.sensitivity_classifications','rank'),
    COL_LENGTH('sys.stats','auto_drop'),COL_LENGTH('sys.stats','has_persisted_sample')";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Features {
    sequential_key: &'static str,
    columnstore_order: &'static str,
    xml_compression: &'static str,
    ledger: &'static str,
    graph: (&'static str, &'static str),
    retention: &'static str,
    suppress_dup_key: &'static str,
    classifications: Option<&'static str>,
    auto_drop: &'static str,
    persisted_sample: &'static str,
    partition_xml_compression: &'static str,
}

impl Features {
    fn from_lengths(row: &[Option<String>]) -> Features {
        let has = |i: usize| row.get(i).is_some_and(Option::is_some);
        let pick = |i: usize, column: &'static str, absent: &'static str| if has(i) { column } else { absent };
        Features {
            sequential_key: pick(0, "i.optimize_for_sequential_key", "0"),
            columnstore_order: pick(1, "ic.column_store_order_ordinal", "0"),
            xml_compression: pick(2, "COALESCE(p.xml_compression,0)", "0"),
            ledger: pick(3, "t.ledger_type<>0", "1=0"),
            graph: if has(4) { ("t.is_node=1", "t.is_edge=1") } else { ("1=0", "1=0") },
            retention: pick(5, "t.history_retention_period,t.history_retention_period_unit_desc", "NULL,NULL"),
            suppress_dup_key: pick(6, "i.suppress_dup_key_messages", "0"),
            classifications: has(7).then(|| pick(8, "sc.rank_desc", "NULL")),
            auto_drop: pick(9, "st.auto_drop", "0"),
            persisted_sample: pick(10, "st.has_persisted_sample", "0"),
            partition_xml_compression: pick(2, "pp.xml_compression", "0"),
        }
    }
}

fn relation_query(oid: i32, f: &Features) -> String {
    format!(
        "SELECT s.name,o.name,o.type,t.uses_ansi_nulls,t.lock_escalation_desc,hs.name,h.name,{},
            ts.name,tt.name,lds.type,lds.name,COL_NAME(pe.object_id,pe.start_column_id),COL_NAME(pe.object_id,pe.end_column_id),
            CASE WHEN t.is_memory_optimized=1 THEN 'Memory-optimized tables' WHEN {} THEN 'Ledger tables' WHEN {} THEN 'Graph node tables'
                 WHEN {} THEN 'Graph edge tables' WHEN t.is_filetable=1 THEN 'FileTables' WHEN t.is_external=1 THEN 'External tables' END,
            NULLIF(t.text_in_row_limit,0),t.large_value_types_out_of_row,ct.is_track_columns_updated_on
         FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
         LEFT JOIN sys.tables t ON t.object_id=o.object_id
         LEFT JOIN sys.objects h ON h.object_id=t.history_table_id LEFT JOIN sys.schemas hs ON hs.schema_id=h.schema_id
         LEFT JOIN sys.tables tt ON tt.history_table_id=o.object_id LEFT JOIN sys.schemas ts ON ts.schema_id=tt.schema_id
         LEFT JOIN sys.data_spaces lds ON lds.data_space_id=t.lob_data_space_id
         LEFT JOIN sys.periods pe ON pe.object_id=o.object_id
         LEFT JOIN sys.change_tracking_tables ct ON ct.object_id=o.object_id
         WHERE o.object_id={oid}",
        f.retention, f.ledger, f.graph.0, f.graph.1
    )
}

fn storage_columns(f: &Features) -> String {
    format!(
        "i.fill_factor,i.is_padded,i.ignore_dup_key,COALESCE(st.no_recompute,0),i.allow_row_locks,i.allow_page_locks,{},NULLIF(NULLIF(p.data_compression_desc,'NONE'),'COLUMNSTORE'),{},i.compression_delay,i.is_disabled,ds.type,ds.name,COALESCE(st.is_incremental,0),{},
            CASE WHEN (SELECT COUNT(DISTINCT CONCAT(pp.data_compression,'/',{})) FROM sys.partitions pp WHERE pp.object_id=i.object_id AND pp.index_id=i.index_id)>1 THEN 1 ELSE 0 END",
        f.sequential_key, f.xml_compression, f.suppress_dup_key, f.partition_xml_compression
    )
}

const STORAGE_JOINS: &str = "LEFT JOIN sys.stats st ON st.object_id=i.object_id AND st.stats_id=i.index_id
     OUTER APPLY (SELECT TOP 1 * FROM sys.partitions p WHERE p.object_id=i.object_id AND p.index_id=i.index_id ORDER BY p.partition_number) p
     JOIN sys.data_spaces ds ON ds.data_space_id=i.data_space_id";

fn place(row: &[Option<String>], at: usize) -> Option<ddl::Place> {
    match (row.get(at)?.as_deref()?.trim(), row.get(at + 1)?.clone()?) {
        ("FG", name) if name == "PRIMARY" => None,
        ("PS", name) => Some(ddl::Place::PartitionScheme(name)),
        (_, name) => Some(ddl::Place::Filegroup(name)),
    }
}

fn storage(r: &[Option<String>], at: usize) -> Result<ddl::Storage, DetailError> {
    Ok(ddl::Storage {
        fill_factor: number(r, at).map_err(failed)?,
        pad_index: bit(r, at + 1)?,
        ignore_dup_key: bit(r, at + 2)?,
        no_recompute: bit(r, at + 3)?,
        deny_row_locks: !bit(r, at + 4)?,
        deny_page_locks: !bit(r, at + 5)?,
        sequential_key: bit(r, at + 6)?,
        compression: r[at + 7].clone(),
        xml_compression: bit(r, at + 8)?,
        compression_delay: r[at + 9].as_deref().and_then(|d| d.parse().ok()),
        disabled: bit(r, at + 10)?,
        place: place(r, at + 11),
        unscripted: ddl::Unscripted {
            incremental: bit(r, at + 13)?,
            suppress_dup_key_messages: bit(r, at + 14)?,
            mixed_compression: bit(r, at + 15)?,
        },
    })
}

type Row = Vec<Option<String>>;

#[derive(Default)]
struct Batch(Vec<String>);

impl Batch {
    fn add(&mut self, sql: String) -> usize {
        self.0.push(sql);
        self.0.len() - 1
    }
}

impl MsSession {
    pub(in crate::session) async fn script(&mut self, target: &ScriptTarget) -> Result<ObjectScript, DetailError> {
        let f = self.features().await?;
        match target {
            ScriptTarget::Relation { oid } => self.relation_script(native_object_id(*oid), &f).await,
            ScriptTarget::Routine { oid } => {
                self.module_script(&format!("o.object_id={}", native_object_id(*oid)), f).await
            }
            ScriptTarget::Trigger { relation_oid, name } => {
                self.module_script(
                    &format!(
                        "o.parent_object_id={} AND o.type IN ('TR','TA') AND o.name={}",
                        native_object_id(*relation_oid),
                        quote_literal(name)
                    ),
                    f,
                )
                .await
            }
            ScriptTarget::Index { relation_oid, name } => {
                let oid = native_object_id(*relation_oid);
                let literal = quote_literal(name);
                let mut batch = Batch::default();
                let table = batch.add(format!("SELECT s.name,o.name FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id WHERE o.object_id={oid}"));
                let props = batch.add(properties_sql(&format!("((ep.class=7 AND ep.major_id={oid} AND i.name={literal}) OR (ep.class=1 AND c.parent_object_id={oid} AND c.name={literal}))")));
                let keys = batch.add(keys_sql(oid, &format!("k.name={literal}"), &f));
                let indexes = batch.add(indexes_sql(oid, &format!("i.name={literal}"), &f));
                let sets = self.run_batch(batch).await?;
                let table = sets[table].first().ok_or(DetailError::Gone)?;
                let table = ddl::qualified(&text(table, 0)?, &text(table, 1)?);
                let props = properties(&sets[props])?;
                if let Some(key) = keys_from(&sets[keys])?.first() {
                    return Ok(script::object_script(name, ddl::key_script(key, &table, &props)));
                }
                let indexes = indexes_from(&sets[indexes])?;
                let index = indexes.first().ok_or(DetailError::Gone)?;
                if index.unreadable_filter {
                    return Err(refused(format!(
                        "SQL Server hides the filter of {name} from this login, which lacks VIEW DEFINITION permission."
                    )));
                }
                let ddl = ddl::index(index, &table, &props).ok_or_else(|| refused(format!("Only clustered, nonclustered, and columnstore indexes can be scripted, and {name} is none of these.")))?;
                Ok(script::object_script(name, ddl))
            }
            ScriptTarget::Sequence { oid } => self.sequence_script(native_object_id(*oid)).await,
            ScriptTarget::Type { oid } => self.type_script(native_object_id(*oid)).await,
            ScriptTarget::Synonym { oid } => self.synonym_script(native_object_id(*oid)).await,
            ScriptTarget::Extension { .. } => Err(refused("SQL Server has no extensions.")),
        }
    }

    async fn object_warnings(
        &mut self,
        oid: i32,
        class: u8,
    ) -> Result<(Vec<Privilege>, Vec<ddl::Omitted>), DetailError> {
        let f = self.features().await?;
        let mut batch = Batch::default();
        let privileges = if class == 1 {
            batch.add(privileges_sql(&format!("o.object_id={oid}")))
        } else {
            batch.add(format!(
                "SELECT p.permission_name,COL_NAME(p.major_id,p.minor_id),dp.name,p.state FROM sys.database_permissions p JOIN sys.database_principals dp ON dp.principal_id=p.grantee_principal_id WHERE p.class={class} AND p.major_id={oid} ORDER BY dp.name,p.permission_name,p.minor_id"
            ))
        };
        let owner = if class == 1 {
            batch.add(format!(
                "SELECT COALESCE(USER_NAME(o.principal_id),CONVERT(sysname,o.principal_id)) FROM sys.objects o WHERE o.object_id={oid} AND o.principal_id IS NOT NULL AND o.principal_id<>(SELECT s.principal_id FROM sys.schemas s WHERE s.schema_id=o.schema_id)"
            ))
        } else {
            batch.add(format!(
                "SELECT COALESCE(USER_NAME(t.principal_id),CONVERT(sysname,t.principal_id)) FROM sys.types t WHERE t.user_type_id={oid} AND t.principal_id IS NOT NULL AND t.principal_id<>(SELECT s.principal_id FROM sys.schemas s WHERE s.schema_id=t.schema_id)"
            ))
        };
        let visibility = batch.add(visibility_sql(&f));
        let sets = self.run_batch(batch).await?;
        let mut omitted = Vec::new();
        if let Some(row) = sets[owner].first() {
            omitted.push(ddl::Omitted::Owner(text(row, 0)?));
        }
        omitted.extend(unseen(&sets[visibility], false)?);
        Ok((privileges_from(&sets[privileges])?, omitted))
    }

    async fn sequence_script(&mut self, oid: i32) -> Result<ObjectScript, DetailError> {
        let rows = self
            .rows(&format!(
                "SELECT s.name, seq.name, TYPE_NAME(seq.system_type_id), seq.precision, seq.scale, CONVERT(nvarchar(40), seq.start_value), CONVERT(nvarchar(40), seq.increment), CONVERT(nvarchar(40), seq.minimum_value), CONVERT(nvarchar(40), seq.maximum_value), seq.is_cached, seq.cache_size, seq.is_cycling FROM sys.sequences seq JOIN sys.schemas s ON s.schema_id=seq.schema_id WHERE seq.object_id={oid}"
            ))
            .await
            .map_err(failed)?;
        let row = rows.first().ok_or(DetailError::Gone)?;
        let type_name = text(row, 2)?;
        let cache = if bit(row, 9)? {
            match row.get(10).and_then(Option::as_deref) {
                Some(size) => ddl::SequenceCache::Size(size.to_owned()),
                None => ddl::SequenceCache::Default,
            }
        } else {
            ddl::SequenceCache::Off
        };
        let (privileges, omitted) = self.object_warnings(oid, 1).await?;
        let name = text(row, 1)?;
        let ddl = ddl::sequence_script(&ddl::SequenceScript {
            schema: text(row, 0)?,
            name: name.clone(),
            type_name: display(&type_name, 0, number(row, 3).map_err(failed)?, number(row, 4).map_err(failed)?),
            start: text(row, 5)?,
            increment: text(row, 6)?,
            min: text(row, 7)?,
            max: text(row, 8)?,
            cache,
            cycle: bit(row, 11)?,
            privileges,
            omitted,
        });
        Ok(script::object_script(&name, ddl))
    }

    async fn synonym_script(&mut self, oid: i32) -> Result<ObjectScript, DetailError> {
        let rows = self.rows(&format!("SELECT s.name, syn.name, syn.base_object_name FROM sys.synonyms syn JOIN sys.schemas s ON s.schema_id=syn.schema_id WHERE syn.object_id={oid}")).await.map_err(failed)?;
        let row = rows.first().ok_or(DetailError::Gone)?;
        let (privileges, omitted) = self.object_warnings(oid, 1).await?;
        let name = text(row, 1)?;
        Ok(script::object_script(
            &name,
            ddl::synonym_script(&text(row, 0)?, &name, &text(row, 2)?, &privileges, &omitted),
        ))
    }

    async fn type_script(&mut self, oid: i32) -> Result<ObjectScript, DetailError> {
        let rows = self
            .rows(&format!(
                "SELECT s.name, t.name, t.is_table_type, t.is_nullable, TYPE_NAME(t.system_type_id), t.max_length, t.precision, t.scale, t.is_assembly_type FROM sys.types t JOIN sys.schemas s ON s.schema_id=t.schema_id WHERE t.user_type_id={oid} AND t.is_user_defined=1"
            ))
            .await
            .map_err(failed)?;
        let row = rows.first().ok_or(DetailError::Gone)?;
        if bit(row, 8)? {
            return Err(refused("Assembly types cannot be scripted."));
        }
        let schema = text(row, 0)?;
        let name = text(row, 1)?;
        let (privileges, omitted) = self.object_warnings(oid, 6).await?;
        if bit(row, 2)? {
            let columns = self
                .rows(&format!(
                    "SELECT c.name, COALESCE(TYPE_NAME(c.system_type_id),'unknown'), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, c.is_computed, c.default_object_id FROM sys.columns c JOIN sys.types t ON t.type_table_object_id=c.object_id WHERE t.user_type_id={oid} ORDER BY c.column_id"
                ))
                .await
                .map_err(failed)?;
            let columns = columns
                .iter()
                .map(|r| {
                    let base = text(r, 1)?;
                    Ok(ddl::TableTypeColumn {
                        name: text(r, 0)?,
                        type_name: display(
                            &base,
                            number(r, 2).map_err(failed)?,
                            number(r, 3).map_err(failed)?,
                            number(r, 4).map_err(failed)?,
                        ),
                        nullable: bit(r, 5)?,
                        identity: bit(r, 6)?,
                        computed: bit(r, 7)?,
                        defaulted: number::<i32>(r, 8).map_err(failed)? != 0,
                    })
                })
                .collect::<Result<Vec<_>, DetailError>>()?;
            let mut extra = Vec::new();
            let qualified = ddl::qualified(&schema, &name);
            let indexes = self
                .rows(&format!(
                    "SELECT i.name, i.is_primary_key, i.is_unique_constraint FROM sys.indexes i JOIN sys.types t ON t.type_table_object_id=i.object_id WHERE t.user_type_id={oid} AND i.index_id>0 ORDER BY i.name"
                ))
                .await
                .map_err(failed)?;
            for index in &indexes {
                let kind = if bit(index, 1)? {
                    "primary key"
                } else if bit(index, 2)? {
                    "unique constraint"
                } else {
                    "index"
                };
                extra.push(format!("{kind} {} on {qualified}", quote_ident(&text(index, 0)?)));
            }
            let checks = self.rows(&format!("SELECT cc.name FROM sys.check_constraints cc JOIN sys.types t ON t.type_table_object_id=cc.parent_object_id WHERE t.user_type_id={oid} ORDER BY cc.name")).await.map_err(failed)?;
            for check in &checks {
                extra.push(format!("check constraint {} on {qualified}", quote_ident(&text(check, 0)?)));
            }
            return Ok(script::object_script(
                &name,
                ddl::table_type(&schema, &name, &columns, &extra, &privileges, &omitted),
            ));
        }
        let base = text(row, 4)?;
        let base = display(
            &base,
            number(row, 5).map_err(failed)?,
            number(row, 6).map_err(failed)?,
            number(row, 7).map_err(failed)?,
        );
        Ok(script::object_script(&name, ddl::alias_type(&schema, &name, &base, bit(row, 3)?, &privileges, &omitted)))
    }

    async fn features(&mut self) -> Result<Features, DetailError> {
        if let Some(f) = self.script_features {
            return Ok(f);
        }
        let rows = self.rows(FEATURES).await.map_err(failed)?;
        let f = Features::from_lengths(rows.first().map_or(&[][..], Vec::as_slice));
        self.script_features = Some(f);
        Ok(f)
    }

    async fn run_batch(&mut self, batch: Batch) -> Result<Vec<Vec<Row>>, DetailError> {
        let sets = self.results(&batch.0.join(";\n")).await.map_err(failed)?;
        if sets.len() != batch.0.len() {
            return Err(refused(format!(
                "SQL Server returned {} result sets for {} queries.",
                sets.len(),
                batch.0.len()
            )));
        }
        Ok(sets)
    }

    async fn relation_script(&mut self, oid: i32, f: &Features) -> Result<ObjectScript, DetailError> {
        let mut batch = Batch::default();
        let relation = batch.add(relation_query(oid, f));
        let view = batch.add(modules_sql(&format!("o.object_id={oid}")));
        let omitted = batch
            .add(omitted_sql(&format!("o.object_id={oid} OR (o.parent_object_id={oid} AND o.type IN ('TR','TA'))")));
        let heap = batch.add(format!(
            "SELECT {} FROM sys.indexes i {STORAGE_JOINS} WHERE i.object_id={oid} AND i.index_id=0",
            storage_columns(f)
        ));
        let columns = batch.add(columns_sql(oid));
        let keys = batch.add(keys_sql(oid, "1=1", f));
        let foreign_keys = batch.add(foreign_keys_sql(oid));
        let checks = batch.add(format!("SELECT name,definition,is_disabled,is_not_trusted,is_not_for_replication FROM sys.check_constraints WHERE parent_object_id={oid} ORDER BY name"));
        let indexes = batch.add(indexes_sql(oid, "i.is_primary_key=0 AND i.is_unique_constraint=0", f));
        let statistics = batch.add(statistics_sql(oid, f));
        let triggers = batch.add(modules_sql(&format!("o.parent_object_id={oid} AND o.type IN ('TR','TA')")));
        let props = batch.add(properties_sql(&format!("o.object_id={oid}")));
        let privileges = batch.add(privileges_sql(&format!("o.object_id={oid}")));
        let classifications = f.classifications.map(|rank| batch.add(classifications_sql(oid, rank)));
        let visibility = batch.add(visibility_sql(f));
        let sets = self.run_batch(batch).await?;
        let omitted = omitted_from(&sets[omitted])?;

        let r = sets[relation].first().ok_or(DetailError::Gone)?;
        match required(r, 2).map_err(failed)?.trim() {
            "V" => {
                let (_, module) = modules(&sets[view], &omitted)?.into_iter().next().ok_or(DetailError::Gone)?;
                let mut module = module.text()?;
                module.omitted.extend(unseen(&sets[visibility], false)?);
                let view = ddl::View {
                    indexes: indexes_from(&sets[indexes])?,
                    statistics: statistics_from(&sets[statistics])?,
                    triggers: triggers_from(&sets[triggers], &omitted)?,
                    properties: properties(&sets[props])?,
                    privileges: privileges_from(&sets[privileges])?,
                    module,
                };
                Ok(script::object_script(&view.module.name, ddl::view(&view)))
            }
            "U" => {
                if let Some(kind) = &r[15] {
                    return Err(refused(format!("{kind} cannot be scripted yet.")));
                }
                let name = text(r, 1)?;
                let versioning = match (&r[5], &r[6]) {
                    (Some(schema), Some(history)) => Some(ddl::Versioning {
                        schema: schema.clone(),
                        name: history.clone(),
                        retention: match (r[7].as_deref().and_then(|n| n.parse::<i32>().ok()), r[8].as_deref()) {
                            (Some(n), Some(unit)) if n >= 0 && unit != "INFINITE" => Some((n, unit.to_owned())),
                            _ => None,
                        },
                    }),
                    _ => None,
                };
                let table = ddl::Table {
                    schema: text(r, 0)?,
                    name: name.clone(),
                    ansi_nulls: bit(r, 3)?,
                    columns: columns_from(&sets[columns])?,
                    keys: keys_from(&sets[keys])?,
                    foreign_keys: foreign_keys_from(&sets[foreign_keys])?,
                    checks: sets[checks]
                        .iter()
                        .map(|r| Ok(ddl::Check { name: text(r, 0)?, definition: r[1].clone(), state: state(r, 2)? }))
                        .collect::<Result<_, DetailError>>()?,
                    indexes: indexes_from(&sets[indexes])?,
                    statistics: statistics_from(&sets[statistics])?,
                    triggers: triggers_from(&sets[triggers], &omitted)?,
                    heap: sets[heap].first().map(|r| storage(r, 0)).transpose()?,
                    large_values: place(r, 11),
                    lock_escalation: text(r, 4)?,
                    text_in_row: r[16].as_deref().and_then(|n| n.parse().ok()),
                    large_values_out_of_row: bit(r, 17)?,
                    change_tracking: r[18].as_deref().map(|on| on == "1"),
                    period: r[13].clone().zip(r[14].clone()),
                    versioning,
                    history_of: r[9].clone().zip(r[10].clone()),
                    properties: properties(&sets[props])?,
                    classifications: classifications.map_or(Ok(vec![]), |at| classifications_from(&sets[at]))?,
                    privileges: privileges_from(&sets[privileges])?,
                    omitted: omitted
                        .into_iter()
                        .filter(|(of, _)| *of == oid)
                        .map(|(_, o)| o)
                        .chain(unseen(&sets[visibility], classifications.is_some())?)
                        .collect(),
                };
                Ok(script::object_script(&name, ddl::table(&table)))
            }
            _ => Err(refused("Only tables and views can be scripted.")),
        }
    }

    async fn module_script(&mut self, filter: &str, f: Features) -> Result<ObjectScript, DetailError> {
        let mut batch = Batch::default();
        let found = batch.add(modules_sql(filter));
        let omitted = batch.add(omitted_sql(filter));
        let props = batch.add(properties_sql(&format!(
            "ep.class IN (1,2) AND ep.major_id IN (SELECT o.object_id FROM sys.objects o WHERE {filter})"
        )));
        let privileges = batch.add(privileges_sql(filter));
        let visibility = batch.add(visibility_sql(&f));
        let sets = self.run_batch(batch).await?;
        let (_, module) =
            modules(&sets[found], &omitted_from(&sets[omitted])?)?.into_iter().next().ok_or(DetailError::Gone)?;
        let mut module = module.text()?;
        module.properties = properties(&sets[props])?;
        module.privileges = privileges_from(&sets[privileges])?;
        module.omitted.extend(unseen(&sets[visibility], false)?);
        Ok(script::object_script(&module.name, ddl::module(&module)))
    }
}

fn modules_sql(filter: &str) -> String {
    format!(
        "SELECT s.name,o.name,OBJECT_DEFINITION(o.object_id),m.uses_ansi_nulls,m.uses_quoted_identifier,tr.is_disabled,ps.name,po.name,o.object_id,o.type,
            STUFF((SELECT ','+CASE WHEN te.is_first=1 THEN 'First' ELSE 'Last' END+' '+te.type_desc FROM sys.trigger_events te
                   WHERE te.object_id=o.object_id AND (te.is_first=1 OR te.is_last=1) ORDER BY te.type FOR XML PATH('')),1,1,'')
         FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id
         LEFT JOIN sys.sql_modules m ON m.object_id=o.object_id
         LEFT JOIN sys.triggers tr ON tr.object_id=o.object_id
         LEFT JOIN sys.objects po ON po.object_id=o.parent_object_id LEFT JOIN sys.schemas ps ON ps.schema_id=po.schema_id
         WHERE {filter} ORDER BY o.name"
    )
}

/// The properties of each object in `filter` that the script warns about instead of writing.
fn omitted_sql(filter: &str) -> String {
    format!(
        "SELECT o.object_id,x.kind,x.name FROM sys.objects o CROSS APPLY (
            SELECT 'owner',COALESCE(USER_NAME(o.principal_id),CONVERT(sysname,o.principal_id))
            WHERE o.principal_id IS NOT NULL AND o.principal_id<>(SELECT s.principal_id FROM sys.schemas s WHERE s.schema_id=o.schema_id)
            UNION ALL SELECT 'runs as owner',USER_NAME(OBJECTPROPERTY(o.object_id,'OwnerId')) FROM sys.sql_modules m WHERE m.object_id=o.object_id AND m.execute_as_principal_id=-2
            UNION ALL SELECT 'change data capture',NULL FROM sys.tables t WHERE t.object_id=o.object_id AND t.is_tracked_by_cdc=1
            UNION ALL SELECT 'full-text index',NULL FROM sys.fulltext_indexes fi WHERE fi.object_id=o.object_id
            UNION ALL SELECT 'hypothetical index',i.name FROM sys.indexes i WHERE i.object_id=o.object_id AND i.is_hypothetical=1
            UNION ALL SELECT 'numbered procedure',CONVERT(sysname,np.procedure_number) FROM sys.numbered_procedures np WHERE np.object_id=o.object_id
            UNION ALL SELECT LOWER(cp.crypt_type_desc),COALESCE(c.name,ak.name,CONVERT(sysname,cp.thumbprint,1)) FROM sys.crypt_properties cp
                LEFT JOIN sys.certificates c ON c.thumbprint=cp.thumbprint LEFT JOIN sys.asymmetric_keys ak ON ak.thumbprint=cp.thumbprint
                WHERE cp.class=1 AND cp.major_id=o.object_id
         ) x(kind,name)
         WHERE {filter} ORDER BY o.object_id,x.kind,x.name"
    )
}

fn omitted_from(rows: &[Row]) -> Result<Vec<(i32, ddl::Omitted)>, DetailError> {
    rows.iter()
        .map(|r| {
            let name = r[2].clone().unwrap_or_default();
            let omitted = match required(r, 1).map_err(failed)? {
                "owner" => ddl::Omitted::Owner(name),
                "runs as owner" => ddl::Omitted::RunsAsOwner(name),
                "change data capture" => ddl::Omitted::ChangeDataCapture,
                "full-text index" => ddl::Omitted::FullTextIndex,
                "hypothetical index" => ddl::Omitted::HypotheticalIndex(name),
                "numbered procedure" => ddl::Omitted::NumberedProcedure(name),
                signature => ddl::Omitted::Signature { kind: signature.to_owned(), by: name },
            };
            Ok((number(r, 0).map_err(failed)?, omitted))
        })
        .collect()
}

fn modules(rows: &[Row], omitted: &[(i32, ddl::Omitted)]) -> Result<Vec<(i32, Found)>, DetailError> {
    rows.iter()
        .map(|r| {
            let (schema, name) = (text(r, 0)?, text(r, 1)?);
            let oid = number(r, 8).map_err(failed)?;
            if matches!(required(r, 9).map_err(failed)?.trim(), "TA" | "PC" | "FS" | "FT") {
                return Ok((oid, Found::Clr { schema, name }));
            }
            let Some(definition) = r[2].clone() else {
                return Ok((oid, Found::Unreadable { schema, name }));
            };
            let disabled_on = match r[5].as_deref() {
                Some("1") => Some(ddl::qualified(&text(r, 6)?, &text(r, 7)?)),
                _ => None,
            };
            let parent = r[6].clone().zip(r[7].clone()).filter(|_| matches!(required(r, 9).map(str::trim), Ok("TR")));
            let order = r[10].as_deref().map_or(vec![], |o| {
                o.split(',')
                    .filter_map(|e| e.split_once(' '))
                    .map(|(when, event)| (when.to_owned(), event.to_owned()))
                    .collect()
            });
            let omitted = omitted.iter().filter(|(of, _)| *of == oid).map(|(_, o)| o.clone()).collect();
            let module = ddl::Module {
                schema,
                name,
                definition,
                ansi_nulls: bit(r, 3)?,
                quoted_identifier: bit(r, 4)?,
                disabled_on,
                order,
                parent,
                properties: vec![],
                privileges: vec![],
                omitted,
            };
            Ok((oid, Found::Text(module)))
        })
        .collect()
}

fn triggers_from(rows: &[Row], omitted: &[(i32, ddl::Omitted)]) -> Result<Vec<ddl::Trigger>, DetailError> {
    Ok(modules(rows, omitted)?
        .into_iter()
        .map(|(_, m)| match m {
            Found::Text(module) => ddl::Trigger::Text(module),
            Found::Clr { schema, name } => ddl::Trigger::Clr { schema, name },
            Found::Unreadable { schema, name } => ddl::Trigger::Encrypted { schema, name },
        })
        .collect())
}

fn properties_sql(filter: &str) -> String {
    format!(
        "SELECT ep.name,CONVERT(nvarchar(max),ep.value),CONVERT(sysname,SQL_VARIANT_PROPERTY(ep.value,'BaseType')),s.name,
            CASE o.type WHEN 'U' THEN 'TABLE' WHEN 'V' THEN 'VIEW' WHEN 'P' THEN 'PROCEDURE' WHEN 'PC' THEN 'PROCEDURE' ELSE 'FUNCTION' END,o.name,
            CASE WHEN ep.class=7 THEN 'INDEX' WHEN ep.class=2 THEN 'PARAMETER' WHEN ep.minor_id>0 THEN 'COLUMN' WHEN c.type='TR' THEN 'TRIGGER' WHEN c.object_id IS NOT NULL THEN 'CONSTRAINT' END,
            CASE WHEN ep.class=7 THEN i.name WHEN ep.class=2 THEN pa.name WHEN ep.minor_id>0 THEN col.name ELSE c.name END
         FROM sys.extended_properties ep
         LEFT JOIN sys.objects c ON ep.class=1 AND ep.minor_id=0 AND c.object_id=ep.major_id AND c.parent_object_id<>0
         JOIN sys.objects o ON o.object_id=COALESCE(c.parent_object_id,ep.major_id)
         JOIN sys.schemas s ON s.schema_id=o.schema_id
         LEFT JOIN sys.indexes i ON ep.class=7 AND i.object_id=ep.major_id AND i.index_id=ep.minor_id
         LEFT JOIN sys.parameters pa ON ep.class=2 AND pa.object_id=ep.major_id AND pa.parameter_id=ep.minor_id
         LEFT JOIN sys.columns col ON ep.class=1 AND ep.minor_id>0 AND col.object_id=ep.major_id AND col.column_id=ep.minor_id
         WHERE ep.class IN (1,2,7) AND {filter}
         ORDER BY 7,8,1"
    )
}

fn properties(rows: &[Row]) -> Result<Vec<ddl::Property>, DetailError> {
    rows.iter()
        .map(|r| {
            let mut levels = vec![("SCHEMA".to_owned(), text(r, 3)?), (text(r, 4)?, text(r, 5)?)];
            if let Some(level) = r[6].clone() {
                levels.push((level, text(r, 7)?));
            }
            Ok(ddl::Property { name: text(r, 0)?, value: r[1].clone(), base_type: r[2].clone(), levels })
        })
        .collect()
}

/// `filter` picks the objects from `sys.objects o`, as `modules_sql` takes it.
fn privileges_sql(filter: &str) -> String {
    format!(
        "SELECT p.permission_name,COL_NAME(p.major_id,p.minor_id),dp.name,p.state
         FROM sys.database_permissions p JOIN sys.database_principals dp ON dp.principal_id=p.grantee_principal_id
         WHERE p.class=1 AND p.major_id IN (SELECT o.object_id FROM sys.objects o WHERE {filter}) ORDER BY dp.name,p.permission_name,p.minor_id"
    )
}

fn privileges_from(rows: &[Row]) -> Result<Vec<Privilege>, DetailError> {
    rows.iter()
        .map(|r| {
            let change = match required(r, 3).map_err(failed)?.trim() {
                "W" => Change::GrantedWithGrantOption,
                "D" => Change::Denied,
                "R" => Change::Revoked,
                _ => Change::Granted,
            };
            Ok(Privilege {
                name: text(r, 0)?,
                column: r[1].as_deref().map(quote_ident),
                grantee: quote_ident(&text(r, 2)?),
                change,
            })
        })
        .collect()
}

/// Grants to a principal this login cannot see, and every classification without VIEW ANY
/// SENSITIVITY CLASSIFICATION, are missing from the catalog views with no sign that they are.
fn visibility_sql(f: &Features) -> String {
    let classifications = if f.classifications.is_some() {
        "HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW ANY SENSITIVITY CLASSIFICATION')"
    } else {
        "1"
    };
    format!("SELECT HAS_PERMS_BY_NAME(DB_NAME(),'DATABASE','VIEW DEFINITION'),{classifications}")
}

fn unseen(rows: &[Row], classified: bool) -> Result<Vec<ddl::Omitted>, DetailError> {
    let r = rows.first().ok_or_else(|| refused("SQL Server returned no permission row."))?;
    let mut omitted = vec![];
    if !bit(r, 0)? {
        omitted.push(ddl::Omitted::UnseenPrivileges);
    }
    if classified && !bit(r, 1)? {
        omitted.push(ddl::Omitted::UnseenClassifications);
    }
    Ok(omitted)
}

fn classifications_sql(oid: i32, rank: &str) -> String {
    format!(
        "SELECT c.name,sc.label,CONVERT(nvarchar(36),sc.label_id),sc.information_type,CONVERT(nvarchar(36),sc.information_type_id),{rank}
         FROM sys.sensitivity_classifications sc JOIN sys.columns c ON c.object_id=sc.major_id AND c.column_id=sc.minor_id
         WHERE sc.class=1 AND sc.major_id={oid} ORDER BY c.column_id"
    )
}

fn classifications_from(rows: &[Row]) -> Result<Vec<ddl::Classification>, DetailError> {
    rows.iter()
        .map(|r| {
            Ok(ddl::Classification {
                column: text(r, 0)?,
                label: r[1].clone(),
                label_id: r[2].clone(),
                information_type: r[3].clone(),
                information_type_id: r[4].clone(),
                rank: r[5].clone(),
            })
        })
        .collect()
}

fn columns_sql(oid: i32) -> String {
    format!(
        "SELECT c.name,COALESCE(t.name,ut.name),ut.is_user_defined,SCHEMA_NAME(ut.schema_id),ut.name,c.max_length,c.precision,c.scale,c.is_nullable,
            CASE WHEN c.collation_name<>CONVERT(sysname,DATABASEPROPERTYEX(DB_NAME(),'Collation')) THEN c.collation_name END,
            CONVERT(nvarchar(40),ic.seed_value),CONVERT(nvarchar(40),ic.increment_value),cc.definition,cc.is_persisted,dc.name,dc.definition,
            COALESCE(ic.is_not_for_replication,0),c.is_sparse,c.is_column_set,c.is_rowguidcol,mc.masking_function,c.generated_always_type,c.is_hidden,
            CASE WHEN c.is_ansi_padded=0 AND t.name IN ('char','varchar','binary','varbinary') THEN 1 ELSE 0 END,c.is_filestream,
            CASE WHEN c.encryption_type IS NULL THEN 0 ELSE 1 END,SCHEMA_NAME(xsc.schema_id),xsc.name,c.is_xml_document,c.is_computed,
            CASE WHEN c.default_object_id<>0 AND dc.object_id IS NULL THEN 1 ELSE 0 END,OBJECT_SCHEMA_NAME(c.default_object_id),OBJECT_NAME(c.default_object_id),
            CASE WHEN c.rule_object_id<>0 THEN 1 ELSE 0 END,OBJECT_SCHEMA_NAME(c.rule_object_id),OBJECT_NAME(c.rule_object_id)
         FROM sys.columns c
         JOIN sys.types ut ON ut.user_type_id=c.user_type_id
         LEFT JOIN sys.types t ON t.user_type_id=c.system_type_id AND t.system_type_id=t.user_type_id
         LEFT JOIN sys.identity_columns ic ON ic.object_id=c.object_id AND ic.column_id=c.column_id
         LEFT JOIN sys.computed_columns cc ON cc.object_id=c.object_id AND cc.column_id=c.column_id
         LEFT JOIN sys.default_constraints dc ON dc.object_id=c.default_object_id
         LEFT JOIN sys.masked_columns mc ON mc.object_id=c.object_id AND mc.column_id=c.column_id
         LEFT JOIN sys.xml_schema_collections xsc ON xsc.xml_collection_id=c.xml_collection_id AND c.xml_collection_id<>0
         WHERE c.object_id={oid} ORDER BY c.column_id"
    )
}

fn columns_from(rows: &[Row]) -> Result<Vec<ddl::Column>, DetailError> {
    rows.iter()
        .map(|r| {
            let type_name = match (&r[26], &r[27]) {
                (Some(schema), Some(collection)) => format!(
                    "xml({} {})",
                    if bit(r, 28)? { "DOCUMENT" } else { "CONTENT" },
                    ddl::qualified(schema, collection)
                ),
                _ if bit(r, 2)? => ddl::qualified(&text(r, 3)?, &text(r, 4)?),
                _ => display(
                    required(r, 1).map_err(failed)?,
                    number(r, 5).map_err(failed)?,
                    number(r, 6).map_err(failed)?,
                    number(r, 7).map_err(failed)?,
                ),
            };
            let computed = match &r[12] {
                Some(definition) => Some(ddl::Computed { definition: definition.clone(), persisted: bit(r, 13)? }),
                None => None,
            };
            Ok(ddl::Column {
                name: text(r, 0)?,
                type_name,
                collation: r[9].clone(),
                nullable: bit(r, 8)?,
                identity: r[10].clone().zip(r[11].clone()),
                not_for_replication: bit(r, 16)?,
                computed,
                default: r[14].clone().zip(r[15].clone()),
                sparse: bit(r, 17)?,
                column_set: bit(r, 18)?,
                rowguidcol: bit(r, 19)?,
                masked: r[20].clone(),
                generated: number(r, 21).map_err(failed)?,
                hidden: bit(r, 22)?,
                ansi_padding_off: bit(r, 23)?,
                filestream: bit(r, 24)?,
                encrypted: bit(r, 25)?,
                unreadable_computed: r[12].is_none() && bit(r, 29)?,
                hidden_default: r[15].is_none().then(|| r[14].clone()).flatten(),
                bound_default: bound(r, 30)?,
                bound_rule: bound(r, 33)?,
            })
        })
        .collect()
}

fn bound(r: &[Option<String>], at: usize) -> Result<Option<ddl::Bound>, DetailError> {
    if !bit(r, at)? {
        return Ok(None);
    }
    Ok(Some(match (r[at + 1].clone(), r[at + 2].clone()) {
        (Some(schema), Some(name)) => ddl::Bound::Object { schema, name },
        _ => ddl::Bound::Hidden,
    }))
}

fn keys_sql(oid: i32, filter: &str, f: &Features) -> String {
    format!(
        "SELECT k.name,k.type,i.type,c.name,ic.is_descending_key,{},c.is_computed FROM sys.key_constraints k
         JOIN sys.indexes i ON i.object_id=k.parent_object_id AND i.index_id=k.unique_index_id
         {STORAGE_JOINS}
         JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id
         JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
         WHERE k.parent_object_id={oid} AND ic.key_ordinal>0 AND {filter}
         ORDER BY CASE k.type WHEN 'PK' THEN 0 ELSE 1 END,k.name,ic.key_ordinal",
        storage_columns(f)
    )
}

fn keys_from(rows: &[Row]) -> Result<Vec<ddl::Key>, DetailError> {
    let mut keys: Vec<ddl::Key> = vec![];
    for r in rows {
        let name = text(r, 0)?;
        let column = (text(r, 3)?, bit(r, 4)?);
        match keys.last_mut() {
            Some(k) if k.name == name => {
                k.columns.push(column);
                k.computed |= bit(r, 21)?;
            }
            _ => keys.push(ddl::Key {
                name,
                primary: required(r, 1).map_err(failed)?.trim() == "PK",
                clustered: required(r, 2).map_err(failed)? == "1",
                columns: vec![column],
                storage: storage(r, 5)?,
                computed: bit(r, 21)?,
            }),
        }
    }
    Ok(keys)
}

fn foreign_keys_sql(oid: i32) -> String {
    format!(
        "SELECT fk.name,c.name,rs.name,ro.name,rc.name,fk.delete_referential_action_desc,fk.update_referential_action_desc,fk.is_disabled,fk.is_not_trusted,fk.is_not_for_replication,
            CASE WHEN fk.referenced_object_id=fk.parent_object_id AND EXISTS (SELECT 1 FROM sys.indexes ri WHERE ri.object_id=fk.referenced_object_id
                 AND ri.index_id=fk.key_index_id AND ri.is_primary_key=0 AND ri.is_unique_constraint=0) THEN 1 ELSE 0 END
         FROM sys.foreign_keys fk JOIN sys.foreign_key_columns fc ON fc.constraint_object_id=fk.object_id
         JOIN sys.columns c ON c.object_id=fc.parent_object_id AND c.column_id=fc.parent_column_id
         JOIN sys.objects ro ON ro.object_id=fc.referenced_object_id JOIN sys.schemas rs ON rs.schema_id=ro.schema_id
         JOIN sys.columns rc ON rc.object_id=fc.referenced_object_id AND rc.column_id=fc.referenced_column_id
         WHERE fk.parent_object_id={oid} ORDER BY fk.name,fc.constraint_column_id"
    )
}

fn foreign_keys_from(rows: &[Row]) -> Result<Vec<ddl::ForeignKey>, DetailError> {
    let mut keys: Vec<ddl::ForeignKey> = vec![];
    for r in rows {
        let name = text(r, 0)?;
        let (column, ref_column) = (text(r, 1)?, text(r, 4)?);
        match keys.last_mut() {
            Some(k) if k.name == name => {
                k.columns.push(column);
                k.ref_columns.push(ref_column);
            }
            _ => keys.push(ddl::ForeignKey {
                name,
                columns: vec![column],
                ref_schema: text(r, 2)?,
                ref_table: text(r, 3)?,
                ref_columns: vec![ref_column],
                on_delete: text(r, 5)?,
                on_update: text(r, 6)?,
                state: state(r, 7)?,
                after_indexes: bit(r, 10)?,
            }),
        }
    }
    Ok(keys)
}

/// Clustered indexes come first, because a view's other indexes need its clustered index.
fn indexes_sql(oid: i32, filter: &str, f: &Features) -> String {
    format!(
        "SELECT i.name,i.is_unique,i.type,i.filter_definition,c.name,ic.is_descending_key,ic.is_included_column,{},o.type,{},c.is_computed,i.has_filter,ic.key_ordinal
         FROM sys.indexes i JOIN sys.objects o ON o.object_id=i.object_id
         {STORAGE_JOINS}
         JOIN sys.index_columns ic ON ic.object_id=i.object_id AND ic.index_id=i.index_id
         JOIN sys.columns c ON c.object_id=ic.object_id AND c.column_id=ic.column_id
         WHERE i.object_id={oid} AND i.index_id>0 AND i.is_hypothetical=0 AND {filter}
         ORDER BY CASE WHEN i.type IN (1,5) THEN 0 ELSE 1 END,i.name,ic.is_included_column,ic.key_ordinal,ic.index_column_id",
        f.columnstore_order,
        storage_columns(f)
    )
}

fn indexes_from(rows: &[Row]) -> Result<Vec<ddl::Index>, DetailError> {
    let mut indexes: Vec<(ddl::Index, Vec<(u32, String)>)> = vec![];
    for r in rows {
        let name = text(r, 0)?;
        let (index, order) = match indexes.last_mut() {
            Some((i, order)) if i.name == name => (i, order),
            _ => {
                let index = ddl::Index {
                    name,
                    kind: number(r, 2).map_err(failed)?,
                    unique: bit(r, 1)?,
                    keys: vec![],
                    include: vec![],
                    order: vec![],
                    filter: r[3].clone(),
                    on_view: required(r, 8).map_err(failed)?.trim() == "V",
                    storage: storage(r, 9)?,
                    computed: false,
                    unreadable_filter: r[3].is_none() && bit(r, 26)?,
                };
                indexes.push((index, vec![]));
                let (i, order) = indexes.last_mut().expect("pushed above");
                (i, order)
            }
        };
        let column = text(r, 4)?;
        let ordinal: u32 = number(r, 7).map_err(failed)?;
        if ordinal > 0 {
            order.push((ordinal, column.clone()));
        }
        // SQL Server lists the partitioning column of an aligned index with key ordinal 0, though
        // it is neither a key nor included.
        let key = number::<u8>(r, 27).map_err(failed)? > 0;
        if bit(r, 6)? {
            index.include.push(column);
        } else if key {
            index.keys.push((column, bit(r, 5)?));
        } else {
            continue;
        }
        index.computed |= bit(r, 25)?;
    }
    Ok(indexes
        .into_iter()
        .map(|(mut index, mut order)| {
            order.sort();
            index.order = order.into_iter().map(|(_, c)| c).collect();
            index
        })
        .collect())
}

fn statistics_sql(oid: i32, f: &Features) -> String {
    format!(
        "SELECT st.name,st.filter_definition,st.has_filter,st.no_recompute,{},st.is_incremental,{},c.name
         FROM sys.stats st JOIN sys.stats_columns sc ON sc.object_id=st.object_id AND sc.stats_id=st.stats_id
         JOIN sys.columns c ON c.object_id=sc.object_id AND c.column_id=sc.column_id
         WHERE st.object_id={oid} AND st.user_created=1 ORDER BY st.name,sc.stats_column_id",
        f.auto_drop, f.persisted_sample
    )
}

fn statistics_from(rows: &[Row]) -> Result<Vec<ddl::Statistics>, DetailError> {
    let mut statistics: Vec<ddl::Statistics> = vec![];
    for r in rows {
        let name = text(r, 0)?;
        let column = text(r, 7)?;
        match statistics.last_mut() {
            Some(s) if s.name == name => s.columns.push(column),
            _ => statistics.push(ddl::Statistics {
                name,
                columns: vec![column],
                filter: r[1].clone(),
                unreadable_filter: r[1].is_none() && bit(r, 2)?,
                no_recompute: bit(r, 3)?,
                auto_drop: bit(r, 4)?,
                incremental: bit(r, 5)?,
                persisted_sample: bit(r, 6)?,
            }),
        }
    }
    Ok(statistics)
}

enum Found {
    Text(ddl::Module),
    Clr { schema: String, name: String },
    Unreadable { schema: String, name: String },
}

impl Found {
    fn text(self) -> Result<ddl::Module, DetailError> {
        match self {
            Found::Text(module) => Ok(module),
            Found::Clr { schema, name } => Err(refused(format!(
                "{} runs CLR code, so SQL Server keeps no T-SQL text for it.",
                ddl::qualified(&schema, &name)
            ))),
            Found::Unreadable { schema, name } => Err(refused(format!(
                "SQL Server has no readable definition for {}. The module is encrypted, or this login lacks VIEW DEFINITION permission.",
                ddl::qualified(&schema, &name)
            ))),
        }
    }
}

fn state(r: &[Option<String>], at: usize) -> Result<ddl::State, DetailError> {
    Ok(ddl::State { disabled: bit(r, at)?, untrusted: bit(r, at + 1)?, not_for_replication: bit(r, at + 2)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_without_a_feature_column_reads_a_constant_in_its_place() {
        let sql2016 = Features::from_lengths(&vec![None; 11]);
        let query = relation_query(7, &sql2016);
        assert!(!query.contains("history_retention_period"), "{query}");
        assert!(query.contains("h.name,NULL,NULL,\n"), "{query}");
        assert!(!query.contains("ledger_type") && !query.contains("is_node"), "{query}");
        let sql2022 = Features::from_lengths(&[
            Some("1".into()),
            Some("4".into()),
            Some("1".into()),
            Some("1".into()),
            Some("1".into()),
            Some("4".into()),
            Some("1".into()),
            Some("256".into()),
            Some("4".into()),
            Some("1".into()),
            Some("1".into()),
        ]);
        assert!(
            relation_query(7, &sql2022)
                .contains("h.name,t.history_retention_period,t.history_retention_period_unit_desc,\n")
        );
        assert_eq!(Features::from_lengths(&[]), sql2016);
    }
}
