use std::sync::Arc;

use engine::Engine;
use engine::model::*;
use tauri::{AppHandle, Manager, State};

#[tauri::command]
#[specta::specta]
pub fn app_boot(engine: State<'_, Arc<Engine>>) -> BootSnapshot {
    engine.boot()
}

#[tauri::command]
#[specta::specta]
pub async fn connection_save(engine: State<'_, Arc<Engine>>, draft: ConnectionDraft) -> Result<ConnectionConfig, SaveError> {
    engine.save_connection(draft).await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_delete(engine: State<'_, Arc<Engine>>, id: ConnectionId) -> Result<(), DisconnectError> {
    engine.delete_connection(id).await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_test(engine: State<'_, Arc<Engine>>, draft: ConnectionDraft) -> Result<ServerInfo, ConnectError> {
    engine.test_connection(draft).await
}

#[tauri::command]
#[specta::specta]
pub fn connection_parse_url(engine: State<'_, Arc<Engine>>, url: String) -> Result<ConnectionDraft, String> {
    engine.parse_connection_url(&url)
}

#[tauri::command]
#[specta::specta]
pub fn ssh_trust_host_key(engine: State<'_, Arc<Engine>>, key: HostKeyId) -> Result<(), String> {
    engine.ssh_trust_host_key(key)
}

#[tauri::command]
#[specta::specta]
pub async fn connection_connect(engine: State<'_, Arc<Engine>>, id: ConnectionId) -> Result<ServerInfo, ConnectError> {
    engine.connect(id).await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_disconnect(engine: State<'_, Arc<Engine>>, id: ConnectionId, force: bool) -> Result<(), DisconnectError> {
    engine.disconnect(id, force).await
}

#[tauri::command]
#[specta::specta]
pub async fn catalog_get(engine: State<'_, Arc<Engine>>, db: DatabaseRef) -> Result<Arc<Catalog>, CatalogError> {
    engine.catalog(db).await
}

#[tauri::command]
#[specta::specta]
pub async fn catalog_refresh(engine: State<'_, Arc<Engine>>, db: DatabaseRef) -> Result<u64, CatalogError> {
    engine.refresh_catalog(db).await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_refresh(engine: State<'_, Arc<Engine>>, id: ConnectionId) -> Result<(), CatalogError> {
    engine.refresh_connection(id).await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_edit_view(engine: State<'_, Arc<Engine>>, id: ConnectionId, edit: ViewEdit) -> Result<(), CatalogError> {
    engine.edit_view(id, edit).await
}

#[tauri::command]
#[specta::specta]
pub fn connection_probe_databases(engine: State<'_, Arc<Engine>>, id: ConnectionId, databases: Vec<String>) {
    engine.probe_databases(id, databases)
}

#[tauri::command]
#[specta::specta]
pub async fn relation_detail(engine: State<'_, Arc<Engine>>, db: DatabaseRef, oid: u32) -> Result<RelationDetail, DetailError> {
    engine.relation_detail(db, oid).await
}

#[tauri::command]
#[specta::specta]
pub async fn object_script(engine: State<'_, Arc<Engine>>, db: DatabaseRef, target: ScriptTarget) -> Result<ObjectScript, DetailError> {
    engine.object_script(db, target).await
}

#[tauri::command]
#[specta::specta]
pub async fn exact_row_count(engine: State<'_, Arc<Engine>>, tab_id: TabId, db: DatabaseRef, schema: String, table: String, query: TableQuery) -> Result<u64, CountError> {
    engine.exact_row_count(tab_id, db, schema, table, query).await
}

#[tauri::command]
#[specta::specta]
pub async fn console_configure(engine: State<'_, Arc<Engine>>, tab_id: TabId, config: ConsoleConfig) -> Result<ConsoleState, ConfigureError> {
    engine.console_configure(tab_id, config).await
}

#[tauri::command]
#[specta::specta]
pub fn script_params(engine: State<'_, Arc<Engine>>, text: String, target: RunTarget, database_kind: DatabaseKind) -> Result<Vec<Param>, RunError> {
    engine.script_params(&text, target, database_kind)
}

#[tauri::command]
#[specta::specta]
pub fn script_outline(engine: State<'_, Arc<Engine>>, text: String, cursor: DocOffset, database_kind: DatabaseKind) -> ScriptOutline {
    engine.script_outline(&text, cursor, database_kind)
}

#[tauri::command]
#[specta::specta]
pub fn completion_context(engine: State<'_, Arc<Engine>>, text: String, cursor: DocOffset, database_kind: DatabaseKind) -> CompletionContext {
    engine.completion_context(&text, cursor, database_kind)
}

#[tauri::command]
#[specta::specta]
pub fn editor_format(engine: State<'_, Arc<Engine>>, text: String, target: RunTarget, database_kind: DatabaseKind) -> Result<FormatEdit, FormatError> {
    engine.editor_format(&text, target, database_kind)
}

#[tauri::command]
#[specta::specta]
pub async fn console_run(engine: State<'_, Arc<Engine>>, tab_id: TabId, request: RunRequest, confirmed: Option<WriteDigest>) -> Result<RunAccepted, RunError> {
    engine.console_run(tab_id, request, confirmed).await
}

// Async commands that borrow `State` must return `Result`; the owned `AppHandle` lifts that.
#[tauri::command]
#[specta::specta]
pub async fn console_cancel(app: AppHandle, tab_id: TabId) {
    app.state::<Arc<Engine>>().console_cancel(tab_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn console_commit(engine: State<'_, Arc<Engine>>, tab_id: TabId, confirmed: Option<WriteDigest>) -> Result<(), TxnError> {
    engine.console_commit(tab_id, confirmed).await
}

#[tauri::command]
#[specta::specta]
pub async fn console_rollback(engine: State<'_, Arc<Engine>>, tab_id: TabId) -> Result<(), TxnError> {
    engine.console_rollback(tab_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn table_apply(engine: State<'_, Arc<Engine>>, tab_id: TabId, changes: TableChanges, confirmed: Option<WriteDigest>) -> Result<ApplySummary, ApplyError> {
    engine.table_apply(tab_id, changes, confirmed).await
}

#[tauri::command]
#[specta::specta]
pub async fn table_import_prepare(engine: State<'_, Arc<Engine>>, tab_id: TabId, target: ImportTarget, file: String) -> Result<ImportSourceId, ImportError> {
    engine.table_import_prepare(tab_id, target, file).await
}

#[tauri::command]
#[specta::specta]
pub async fn table_import_preview(engine: State<'_, Arc<Engine>>, tab_id: TabId, source: ImportSourceId, header: bool) -> Result<ImportPreview, ImportError> {
    engine.table_import_preview(tab_id, source, header).await
}

#[tauri::command]
#[specta::specta]
pub async fn table_import(engine: State<'_, Arc<Engine>>, tab_id: TabId, request: ImportRequest, confirmed: Option<WriteDigest>) -> Result<ImportSummary, ImportError> {
    engine.table_import(tab_id, request, confirmed).await
}

#[tauri::command]
#[specta::specta]
pub async fn table_import_discard(engine: State<'_, Arc<Engine>>, tab_id: TabId, source: ImportSourceId) -> Result<(), ImportError> {
    engine.table_import_discard(tab_id, source).await
}

#[tauri::command]
#[specta::specta]
pub async fn console_close(engine: State<'_, Arc<Engine>>, tab_id: TabId, force: bool) -> Result<(), DisconnectError> {
    engine.console_close(tab_id, force).await
}

#[tauri::command]
#[specta::specta]
pub fn result_page(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, start: u32, len: u32) -> Result<RowPage, ResultError> {
    engine.result_page(result_ref, start, len)
}

#[tauri::command]
#[specta::specta]
pub fn result_cell(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, row: u32, col: u16) -> Result<Option<String>, ResultError> {
    engine.result_cell(result_ref, row, col)
}

#[tauri::command]
#[specta::specta]
pub async fn result_cell_matches(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, cell: ResultCell, query: String) -> Result<bool, ResultError> {
    engine.result_cell_matches(result_ref, cell, query).await
}

#[tauri::command]
#[specta::specta]
pub async fn result_find(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, query: String, direction: ResultFindDirection) -> Result<ResultFind, ResultError> {
    engine.result_find(result_ref, query, direction).await
}

#[tauri::command]
#[specta::specta]
pub async fn result_aggregate(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, rows: RowSpan, cols: ColSpan) -> Result<CellAggregate, ResultError> {
    engine.result_aggregate(result_ref, rows, cols).await
}

#[tauri::command]
#[specta::specta]
pub async fn result_series(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, x: Option<u16>, y: u16, max_points: u32) -> Result<ChartSeries, ResultError> {
    engine.result_series(result_ref, x, y, max_points).await
}

#[tauri::command]
#[specta::specta]
pub async fn result_profile(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, col: u16) -> Result<ColumnProfile, ResultError> {
    engine.result_profile(result_ref, col).await
}

#[tauri::command]
#[specta::specta]
pub fn result_drill(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, point: DrillPoint) -> Result<String, DrillError> {
    engine.result_drill(result_ref, point)
}

#[tauri::command]
#[specta::specta]
pub fn result_plan(engine: State<'_, Arc<Engine>>, result_ref: ResultRef) -> Result<Plan, ResultError> {
    engine.result_plan(result_ref)
}

#[tauri::command]
#[specta::specta]
pub async fn result_export(engine: State<'_, Arc<Engine>>, result_ref: ResultRef, format: ExportFormat, scope: ExportScope, path: String) -> Result<ExportSummary, ExportError> {
    engine.result_export(result_ref, format, scope, path).await
}

#[tauri::command]
#[specta::specta]
pub async fn file_open(engine: State<'_, Arc<Engine>>, path: String) -> Result<FileOpened, FileReadError> {
    engine.file_open(path).await
}

#[tauri::command]
#[specta::specta]
pub async fn file_save(engine: State<'_, Arc<Engine>>, target: SaveTarget, text: String) -> Result<FileSaved, FileWriteError> {
    engine.file_save(target, text).await
}

#[tauri::command]
#[specta::specta]
pub fn history_list(engine: State<'_, Arc<Engine>>, query: HistoryQuery) -> HistoryPage {
    engine.history(query)
}

#[tauri::command]
#[specta::specta]
pub fn history_browse(
    engine: State<'_, Arc<Engine>>,
    query: HistoryBrowseQuery,
) -> Result<HistoryBrowsePage, HistoryError> {
    engine.history_browse(query)
}

#[tauri::command]
#[specta::specta]
pub fn history_group(
    engine: State<'_, Arc<Engine>>,
    query: HistoryGroupQuery,
) -> Result<HistoryGroupPage, HistoryError> {
    engine.history_group(query)
}

#[tauri::command]
#[specta::specta]
pub fn history_clear(engine: State<'_, Arc<Engine>>) {
    engine.clear_history()
}

#[tauri::command]
#[specta::specta]
pub fn saved_query_save(engine: State<'_, Arc<Engine>>, text: String, target: RunTarget, database_kind: DatabaseKind) -> Result<SaveQueryReply, SavedQueryError> {
    engine.saved_query_save(&text, target, database_kind)
}

#[tauri::command]
#[specta::specta]
pub fn saved_query_update(engine: State<'_, Arc<Engine>>, id: SavedQueryId, edit: SavedQueryEdit) -> Result<SavedQueries, SavedQueryError> {
    engine.saved_query_update(id, edit)
}

#[tauri::command]
#[specta::specta]
pub fn saved_query_delete(engine: State<'_, Arc<Engine>>, id: SavedQueryId) -> Result<SavedQueries, SavedQueryError> {
    engine.saved_query_delete(id)
}

#[tauri::command]
#[specta::specta]
pub fn saved_query_used(engine: State<'_, Arc<Engine>>, id: SavedQueryId) -> Result<SavedQueries, SavedQueryError> {
    engine.saved_query_used(id)
}

#[tauri::command]
#[specta::specta]
pub fn workspace_save(engine: State<'_, Arc<Engine>>, workspace: Workspace) {
    engine.save_workspace(workspace)
}

#[tauri::command]
#[specta::specta]
pub fn settings_set(engine: State<'_, Arc<Engine>>, settings: Settings) {
    engine.set_settings(settings)
}

#[tauri::command]
#[specta::specta]
pub async fn assistant_open(engine: State<'_, Arc<Engine>>, chat_id: ChatId) -> Result<ChatView, ChatError> {
    engine.assistant_open(chat_id).await
}

#[tauri::command]
#[specta::specta]
pub fn assistant_configure(engine: State<'_, Arc<Engine>>, chat_id: ChatId, options: ChatOptions) -> Result<ChatState, ChatError> {
    engine.assistant_configure(chat_id, options)
}

#[tauri::command]
#[specta::specta]
pub async fn assistant_send(engine: State<'_, Arc<Engine>>, chat_id: ChatId, request: SendRequest) -> Result<TurnAccepted, SendError> {
    engine.assistant_send(chat_id, request).await
}

#[tauri::command]
#[specta::specta]
pub fn assistant_stop(engine: State<'_, Arc<Engine>>, chat_id: ChatId) {
    engine.assistant_stop(chat_id)
}

#[tauri::command]
#[specta::specta]
pub fn assistant_approve(engine: State<'_, Arc<Engine>>, chat_id: ChatId, approval: ApprovalId, decision: Decision) -> Result<(), ApproveError> {
    engine.assistant_approve(chat_id, approval, decision)
}

#[tauri::command]
#[specta::specta]
pub async fn assistant_delete(app: AppHandle, chat_id: ChatId) {
    app.state::<Arc<Engine>>().assistant_delete(chat_id).await
}

#[tauri::command]
#[specta::specta]
pub fn provider_refresh(engine: State<'_, Arc<Engine>>) {
    engine.provider_refresh()
}

#[tauri::command]
#[specta::specta]
pub async fn usage_limits(app: AppHandle) -> LimitsReport {
    app.state::<Arc<Engine>>().usage_limits().await
}

#[tauri::command]
#[specta::specta]
pub fn update_check(engine: State<'_, Arc<Engine>>) {
    engine.update_check()
}

#[tauri::command]
#[specta::specta]
pub fn update_download(engine: State<'_, Arc<Engine>>, version: String) -> Result<(), UpdateError> {
    engine.update_download(version)
}

#[tauri::command]
#[specta::specta]
pub async fn update_install(app: AppHandle, version: String, accepted: Option<RestartImpact>) -> Result<InstallReply, UpdateError> {
    app.state::<Arc<Engine>>().update_install(version, accepted).await
}

#[tauri::command]
#[specta::specta]
pub fn update_set_track(engine: State<'_, Arc<Engine>>, track: UpdateTrack) -> Result<(), UpdateError> {
    engine.update_set_track(track)
}

#[tauri::command]
#[specta::specta]
pub fn table_script(engine: State<'_, Arc<Engine>>, db: DatabaseRef, schema: String, table: String) -> Result<TableScript, String> {
    engine.table_script(db, &schema, &table)
}

#[tauri::command]
#[specta::specta]
pub fn table_order(engine: State<'_, Arc<Engine>>, db: DatabaseRef, schema: String, table: String, order_by: String, edit: TableOrderEdit) -> Result<TableOrderView, String> {
    engine.table_order(db, &schema, &table, &order_by, edit)
}

#[tauri::command]
#[specta::specta]
pub async fn database_map_neighborhood(engine: State<'_, Arc<Engine>>, db: DatabaseRef, oid: u32, catalog_version: u64, after: Option<MapCursor>) -> Result<MapNeighborhood, MapError> {
    engine.database_map_neighborhood(db, oid, catalog_version, after).await
}

#[tauri::command]
#[specta::specta]
pub async fn database_map_query(engine: State<'_, Arc<Engine>>, db: DatabaseRef, text: String, cursor: DocOffset, catalog_version: u64) -> Result<MapQuery, MapError> {
    engine.database_map_query(db, text, cursor, catalog_version).await
}
