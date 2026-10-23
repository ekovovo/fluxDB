#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateDatabaseRequest {
    pub connection_id: ConnectionId,
    pub name: String,
    /// MySQL 字符集；PostgreSQL 作 ENCODING。
    pub charset: String,
    /// MySQL 排序规则；PostgreSQL 作 LC_COLLATE/LC_CTYPE。
    pub collation: String,
    /// PostgreSQL OWNER（可选，空则不指定）。
    pub owner: String,
    /// PostgreSQL TEMPLATE（可选，空则不指定）。
    pub template: String,
    pub path: Option<PathBuf>,
}

pub trait Connector {
    fn kind(&self) -> DatabaseKind;
    fn test_connection(&self, config: &ConnectionConfig) -> Result<()>;
    fn list_objects(&self, path: Option<&ObjectPath>) -> Result<Vec<ObjectSummary>>;
    fn create_database(&self, _: &CreateDatabaseRequest) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持新建数据库"))
    }
    /// 在指定数据库内新建 schema。
    ///
    /// `database` 是目标物理库（PG 的 schema 属于库而非集群）；为空表示由实现回退到
    /// 连接的维护库。缺这个参数会把 schema 建到维护库而非用户右键的那个库。
    fn create_schema(&self, _: ConnectionId, _database: &str, _: &str) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持新建 schema"))
    }
    fn list_roles(&self, _: ConnectionId) -> Result<Vec<PgRole>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持角色管理"))
    }
    fn create_role(
        &self,
        _: ConnectionId,
        _: &str,
        _: bool,
        _: Option<&str>,
    ) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持创建角色"))
    }
    fn alter_role_password(&self, _: ConnectionId, _: &str, _: &str) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持修改角色密码"))
    }
    fn rename_role(&self, _: ConnectionId, _: &str, _: &str) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持重命名角色"))
    }
    fn drop_role(&self, _: ConnectionId, _: &str) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持删除角色"))
    }
    fn alter_role_options(
        &self,
        _: ConnectionId,
        _: &str,
        _: Option<bool>,
        _: Option<bool>,
        _: Option<bool>,
        _: Option<bool>,
        _: Option<bool>,
        _: Option<bool>,
        _: Option<bool>,
        _: Option<i32>,
        _: Option<&str>,
    ) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持修改角色属性"))
    }
    /// 成员授权。`inherit`/`set` 为 PG16+ 成员级选项（PG≤14 语法不支持，连接器按版本省略）。
    fn grant_role_membership(
        &self,
        _: ConnectionId,
        _: &str,
        _: &str,
        _: bool,
        _: bool,
        _: bool,
    ) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持成员授权"))
    }
    fn revoke_role_membership(&self, _: ConnectionId, _: &str, _: &str) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持撤销成员关系"))
    }
    /// 对象授权：`GRANT <privilege> ON <object> TO <grantee> [WITH GRANT OPTION]`。
    ///
    /// 目标以结构化 `PgObjectGrantScope` 传入而非预渲染 SQL 片段：`ON` 后的对象片段由
    /// connector 负责引用与校验（设计 §12），避免上层把用户输入（尤其函数签名）拼进
    /// 特权 DDL。`privilege` 为权限关键字，`grant_option` 决定可否再授权。
    fn grant_object_privilege(
        &self,
        _: ConnectionId,
        _privilege: &str,
        _scope: &PgObjectGrantScope,
        _grantee: &str,
        _grant_option: bool,
    ) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持对象授权"))
    }
    fn revoke_object_privilege(
        &self,
        _: ConnectionId,
        _privilege: &str,
        _scope: &PgObjectGrantScope,
        _grantee: &str,
    ) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持对象撤销"))
    }
    /// 列成员关系（PG 全选项：admin/inherit/set，版本感知）。
    fn list_role_membership(&self, _: ConnectionId) -> Result<Vec<PgRoleMembership>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持读取成员关系"))
    }
    /// 服务端是否支持成员级 INHERIT/SET 选项（PG16+）。PG≤15 返回 false。
    fn supports_member_options(&self, _: ConnectionId) -> Result<bool> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持探测成员选项版本"))
    }
    /// 列对象权限：(grantee, privilege, grant_option)；grantee 空表示 PUBLIC。
    fn list_relation_grants(
        &self,
        _: ConnectionId,
        _: &str,
        _: &str,
    ) -> Result<Vec<(String, String, bool)>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持读取对象权限"))
    }
    /// 列 PG 对象权限（数据库/schema/表·视图·序列/函数），返回完整读模型
    /// （owner、ACL 是否默认、显式条目含 PUBLIC 与 owner 标记）。为 T27 的对象权限展示提供数据源，
    /// 使 UI 能区分默认权限/直接授权/owner，避免据不完整视图误删未展示的授权。
    fn list_object_grants(
        &self,
        _: ConnectionId,
        _: &PgObjectGrantScope,
    ) -> Result<PgObjectGrants> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "暂不支持读取该对象的权限",
        ))
    }
    /// 某角色对某对象的**有效**权限（owner/直接/PUBLIC/继承统一经 PG 判定）。
    ///
    /// 供 T27 展示直接授权与继承/PUBLIC/owner 的差异，避免把继承/owner 误当可直接撤销的直接授权。
    fn role_effective_grants(
        &self,
        _: ConnectionId,
        _: &PgObjectGrantScope,
        _: &str,
    ) -> Result<Vec<PgEffectivePrivilege>> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "暂不支持读取角色有效权限",
        ))
    }
    /// 渲染 PG 角色变更计划为有序 SQL 列表（预览与执行共用同一渲染规则）。
    ///
    /// `mask_secrets=true` 时密码语句以占位符输出（脱敏预览，不能直接执行）；
    /// false 时输出真实可执行语句（仅供连接器执行路径使用，不得进入日志/历史）。
    fn render_role_plan(
        &self,
        _: ConnectionId,
        _plan: &PgRoleSavePlan,
        _mask_secrets: bool,
    ) -> Result<Vec<String>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持渲染角色变更计划"))
    }
    /// 在**同一数据库会话的单事务**内应用角色变更计划（失败整批回滚）。
    ///
    /// 替代「循环调用单项接口」的伪原子提交：角色 DDL、成员关系与对象 ACL 全部
    /// 在一个 BEGIN/COMMIT 中执行，任何一条失败即回滚，不产生半完成状态。
    fn apply_role_plan(&self, _: ConnectionId, _: &PgRoleSavePlan) -> Result<Vec<String>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持应用角色变更计划"))
    }
    /// 列权限页可选目标：数据库 / schema / 表·视图·序列 / 函数（含签名）。
    /// 关系与函数返回 `schema.name`（函数为 `schema.name(参数类型列表)`）形式字符串。
    fn list_grant_targets(&self, _: ConnectionId, _database: &str) -> Result<PgGrantTargetLists> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持读取权限目标列表"))
    }
    /// SQL Server 用户与权限第一阶段：读取服务器登录名、目标库用户/角色和成员关系。
    fn sqlserver_security_snapshot(
        &self,
        _: ConnectionId,
        _: &str,
    ) -> Result<SqlServerSecuritySnapshot> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "暂不支持读取 SQL Server 用户与权限",
        ))
    }
    /// SQL Server 指定主体的显式服务器/数据库权限（不含继承和固定角色隐含权限）。
    fn sqlserver_principal_permissions(
        &self,
        _: ConnectionId,
        _: &str,
        _: &SqlServerPrincipalRef,
    ) -> Result<Vec<SqlServerPermission>> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "暂不支持读取 SQL Server 权限",
        ))
    }
    /// 脱敏渲染 SQL Server 安全计划，供 UI 预览。
    fn render_sqlserver_security_plan(
        &self,
        _: ConnectionId,
        _plan: &SqlServerSecurityPlan,
    ) -> Result<Vec<String>> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "暂不支持预览 SQL Server 用户与权限变更",
        ))
    }
    /// 在单事务内应用 SQL Server 安全计划；任一语句失败整批回滚。
    fn apply_sqlserver_security_plan(
        &self,
        _: ConnectionId,
        _plan: &SqlServerSecurityPlan,
    ) -> Result<Vec<String>> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "暂不支持应用 SQL Server 用户与权限变更",
        ))
    }
    fn delete_database(&self, _: ConnectionId, _: &str) -> Result<()> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持删除数据库"))
    }

    fn load_data(
        &self,
        path: &ObjectPath,
        offset: u64,
        limit: u64,
        sort: &[SortSpec],
        filters: &[FilterSpec],
    ) -> Result<DataPage>;
    fn preview_data_export(
        &self,
        _: &ObjectPath,
        _: &[String],
        _: &[SortSpec],
        _: &[FilterSpec],
    ) -> Result<DataExportPreview> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持导出预览"))
    }
    /// 一致快照分页导出：逐批回调 `on_page`（返回 false 提前停止），每批前检测 `on_cancel`。
    ///
    /// 默认实现经 `load_data` 逐页（各页独立会话，非跨页一致快照）；PostgreSQL 覆写为单
    /// REPEATABLE READ 事务内分页（见 `pg_export_pages`），保证全量一致且内存有界。供导出驱动接线。
    fn export_pages(
        &self,
        path: &ObjectPath,
        sort: &[SortSpec],
        filters: &[FilterSpec],
        on_cancel: &dyn Fn() -> bool,
        on_page: &mut dyn FnMut(DataPage) -> bool,
    ) -> Result<()> {
        // 默认：按 load_data 逐页（无跨页一致快照，与既有行为一致）。
        let mut offset: u64 = 0;
        loop {
            if on_cancel() {
                return Ok(());
            }
            let batch_size = 4096u64;
            let page = self.load_data(path, offset, batch_size, sort, filters)?;
            let count = page.rows.len() as u64;
            let has_more = page.has_more;
            if !on_page(page) {
                return Ok(());
            }
            offset += count;
            if count == 0 || !has_more {
                return Ok(());
            }
        }
    }
    fn apply_changes(&self, changes: &DataChangeSet) -> Result<AppliedChangeOutcome>;
    fn execute(&self, request: &QueryRequest) -> Result<QueryExecutionResult>;

    /// 是否具备有界写入能力：执行时能在事务内复核对象与真实影响，超限即回滚。
    ///
    /// 这是**连接能力**声明，与 SQL 形状无关；写入风险评审据此决定自动放行时
    /// 走有界提交还是普通写入。默认 false，只有真正实现的连接器覆盖。
    fn supports_bounded_write(&self) -> bool {
        false
    }

    /// 独立事务内重新验证并提交有界 UPDATE；任何无法证明或超限都回滚。
    ///
    /// 复核在提交前完成，因此不提供单独的"预检"入口：预检结果不可信，
    /// 评分阶段只用 [`Self::supports_bounded_write`] 声明连接能力。
    fn execute_bounded_update(
        &self,
        _request: &ParameterizedQueryRequest,
        _should_cancel: &dyn Fn() -> bool,
    ) -> Result<QueryExecutionResult> {
        Err(Error::new(ErrorKind::Unsupported, "该连接尚不支持有界写入提交"))
    }
    /// 参数化执行：**必须**由驱动真实 bind（如 MySQL/SQLite 的 `?`、PostgreSQL 的 `$n`）。
    ///
    /// 默认明确拒绝：不支持绑定的连接器绝不能把参数拼进 SQL（字符串插值＝注入面）。
    fn execute_parameterized(
        &self,
        _request: &ParameterizedQueryRequest,
    ) -> Result<QueryExecutionResult> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "该连接暂不支持参数绑定；请改用内联字面量",
        ))
    }
    fn execute_command_workbench(
        &self,
        request: &CommandWorkbenchRequest,
    ) -> Result<CommandWorkbenchExecution> {
        let _ = request;
        Err(Error::new(
            ErrorKind::Unsupported,
            "该连接暂不支持命令执行器",
        ))
    }
    fn execute_with_progress(
        &self,
        request: &QueryRequest,
        on_summary: &mut dyn FnMut(QueryExecutionSummary),
        _should_cancel: &dyn Fn() -> bool,
    ) -> Result<QueryExecutionResult> {
        let execution = self.execute(request)?;
        for summary in execution.summaries.iter().cloned() {
            on_summary(summary);
        }
        Ok(execution)
    }
    fn list_completion_tables(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: u64,
    ) -> Result<Vec<CompletionTable>> {
        Ok(Vec::new())
    }

    /// 当前数据库内所有可见 schema 的表/视图，用于不限定 schema 的裸表补全。
    fn list_completion_tables_all_schemas(
        &self,
        _: Option<&str>,
        _: &str,
        _: u64,
    ) -> Result<Vec<CompletionTable>> {
        Ok(Vec::new())
    }

    fn list_completion_tables_all_schemas_with_cancel(
        &self,
        database: Option<&str>,
        filter: &str,
        limit: u64,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<CompletionTable>> {
        if should_cancel() {
            return Ok(Vec::new());
        }
        let result = self.list_completion_tables_all_schemas(database, filter, limit)?;
        if should_cancel() {
            return Ok(Vec::new());
        }
        Ok(result)
    }

    fn list_completion_tables_with_cancel(
        &self,
        database: Option<&str>,
        schema: Option<&str>,
        filter: &str,
        limit: u64,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<CompletionTable>> {
        if should_cancel() {
            return Ok(Vec::new());
        }
        let result = self.list_completion_tables(database, schema, filter, limit)?;
        if should_cancel() {
            return Ok(Vec::new());
        }
        Ok(result)
    }

    fn list_completion_columns(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
    ) -> Result<Vec<CompletionColumn>> {
        Ok(Vec::new())
    }

    fn list_completion_columns_with_cancel(
        &self,
        database: Option<&str>,
        schema: Option<&str>,
        table: &str,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<CompletionColumn>> {
        if should_cancel() {
            return Ok(Vec::new());
        }
        let result = self.list_completion_columns(database, schema, table)?;
        if should_cancel() {
            return Ok(Vec::new());
        }
        Ok(result)
    }

    fn list_completion_columns_for_tables(
        &self,
        database: Option<&str>,
        schema: Option<&str>,
        tables: &[String],
    ) -> Result<Vec<CompletionColumn>> {
        let mut columns = Vec::new();
        for table in tables {
            columns.extend(self.list_completion_columns(database, schema, table)?);
        }
        Ok(columns)
    }

    fn list_completion_columns_for_tables_with_cancel(
        &self,
        database: Option<&str>,
        schema: Option<&str>,
        tables: &[String],
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<CompletionColumn>> {
        let mut columns = Vec::new();
        for table in tables {
            if should_cancel() {
                return Ok(Vec::new());
            }
            columns.extend(self.list_completion_columns_with_cancel(
                database,
                schema,
                table,
                should_cancel,
            )?);
        }
        if should_cancel() {
            return Ok(Vec::new());
        }
        Ok(columns)
    }

    fn list_completion_routines(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: u64,
    ) -> Result<Vec<CompletionRoutine>> {
        Ok(Vec::new())
    }

    fn list_completion_routines_with_cancel(
        &self,
        database: Option<&str>,
        schema: Option<&str>,
        filter: &str,
        limit: u64,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<CompletionRoutine>> {
        if should_cancel() {
            return Ok(Vec::new());
        }
        let result = self.list_completion_routines(database, schema, filter, limit)?;
        if should_cancel() {
            return Ok(Vec::new());
        }
        Ok(result)
    }

    fn list_completion_triggers(
        &self,
        _: Option<&str>,
        _: Option<&str>,
        _: &str,
        _: u64,
    ) -> Result<Vec<CompletionTrigger>> {
        Ok(Vec::new())
    }

    fn list_completion_triggers_with_cancel(
        &self,
        database: Option<&str>,
        schema: Option<&str>,
        filter: &str,
        limit: u64,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<CompletionTrigger>> {
        if should_cancel() {
            return Ok(Vec::new());
        }
        let result = self.list_completion_triggers(database, schema, filter, limit)?;
        if should_cancel() {
            return Ok(Vec::new());
        }
        Ok(result)
    }

    fn load_cell_binary(&self, _: &ObjectPath, _: &RowIdentity, _: &str) -> Result<Vec<u8>> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "暂不支持二进制单元格读取",
        ))
    }

    fn list_indexes(&self, _: &ObjectPath) -> Result<Vec<IndexInfo>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持索引元数据"))
    }

    fn list_foreign_keys(&self, _: &ObjectPath) -> Result<Vec<ForeignKeyInfo>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持外键元数据"))
    }

    /// 批量读取多张表的外键，返回 `(源表名, 外键)`；复合键每列占一行。
    ///
    /// 用于 ER 等需要整库关系拓扑的场景，避免逐表 N+1 连接。默认实现逐表调用
    /// `list_foreign_keys`；方言可在单次目录查询内批量实现。
    /// `database`/`schema` 用于限定范围（MySQL/SQLite 用 database，PG 用 schema）。
    fn list_foreign_keys_for_tables(
        &self,
        database: Option<&str>,
        schema: Option<&str>,
        tables: &[String],
    ) -> Result<Vec<(String, ForeignKeyInfo)>> {
        let mut out = Vec::new();
        for table in tables {
            let path = ObjectPath {
                connection_id: ConnectionId(0),
                database: database.map(String::from),
                schema: schema.map(String::from),
                name: table.clone(),
                kind: ObjectKind::Table,
            };
            for fk in self.list_foreign_keys(&path)? {
                out.push((table.clone(), fk));
            }
        }
        Ok(out)
    }

    fn list_foreign_keys_with_cancel(
        &self,
        path: &ObjectPath,
        should_cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<ForeignKeyInfo>> {
        if should_cancel() {
            return Ok(Vec::new());
        }
        let result = self.list_foreign_keys(path)?;
        if should_cancel() {
            return Ok(Vec::new());
        }
        Ok(result)
    }

    fn list_triggers(&self, _: &ObjectPath) -> Result<Vec<TriggerInfo>> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持触发器元数据"))
    }

    /// 一次读取设计表所需的列、索引、外键、触发器与 DDL。
    ///
    /// 默认实现兼容尚未提供批量化目录查询的连接器。设计表打开路径位于后台线程，
    /// 方言实现应尽量覆盖本方法并复用一条连接，避免重复登录和重复元数据查询。
    fn load_table_design_metadata(&self, path: &ObjectPath) -> Result<TableDesignMetadata> {
        let columns = self.list_completion_columns(
            path.database.as_deref(),
            path.schema.as_deref(),
            &path.name,
        )?;
        let indexes = self.list_indexes(path)?;
        let foreign_keys = self.list_foreign_keys(path)?;
        let triggers = self.list_triggers(path)?;
        let ddl = self.table_ddl(path)?;
        Ok(TableDesignMetadata {
            columns,
            indexes,
            foreign_keys,
            triggers,
            ddl,
            details_loaded: true,
        })
    }

    /// 可先返回字段等首屏数据，`details_loaded=false` 表示索引等详情需后台补齐。
    fn load_table_design_base_metadata(
        &self,
        path: &ObjectPath,
    ) -> Result<TableDesignMetadata> {
        self.load_table_design_metadata(path)
    }

    fn table_ddl(&self, _: &ObjectPath) -> Result<String> {
        Err(Error::new(ErrorKind::Unsupported, "暂不支持 DDL 元数据"))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataExportPreview {
    pub sql: String,
    pub row_count: u64,
}
