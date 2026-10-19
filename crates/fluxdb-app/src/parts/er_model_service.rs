// ER 逻辑关系服务层（fluxdb-app，er-design.md §6.5 ErModelService 的本地落地）。
//
// 本文件实现本地逻辑关系目录的 CRUD / 确认 / 拒绝 / usage 判定，端点绑定校验交给宿主
// （desktop/加载流程）据 CatalogSnapshot 构造 `ErUsageContext`。服务不拼 SQL、不执行 DDL，
// 编辑只改 FluxDB 本地模型（§D3）。全部写操作带 expected_revision 乐观锁，避免 Agent 建议
// 覆盖用户刚改的关系（er-design §6.3）。
//
// 存储经 `ErRelationshipStore` trait 注入：生产用 fluxdb-storage 的 FileStorage 适配，
// 单测用内存 Map，保证无数据库也完整验证调用链。

use fluxdb_core::{ErRelationshipReview, ErReviewState, ErUsage, ErUsageContext};

/// 关系目录存储抽象（供注入测试）。Send+Sync：服务在后台线程调用，且目录读写在服务内
/// Mutex 串行，跨操作不丢更新；测试可用多线程验证。
pub trait ErRelationshipStore: Send + Sync {
    fn load(&self, scope_key: &str) -> fluxdb_core::Result<Vec<ErRelationship>>;
    fn save(&self, scope_key: &str, rels: &[ErRelationship]) -> fluxdb_core::Result<()>;
}

/// 关系写操作/校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErModelError {
    /// 关系目录读写失败；不能降级为空列表或伪造成功。
    Storage(String),
    NotFound,
    /// expected_revision 与当前不匹配（并发/过期编辑，§6.3）。
    RevisionConflict,
    /// column_pairs / required_filters 结构性非法（空、重复、字面量滥用）。
    InvalidDefinition(String),
    /// (left, right) 内 role 重名（§5.3 role 对内唯一）。
    DuplicateRole,
}

impl std::fmt::Display for ErModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErModelError::Storage(reason) => write!(f, "关系目录存储失败：{reason}"),
            ErModelError::NotFound => write!(f, "关系不存在"),
            ErModelError::RevisionConflict => write!(f, "关系已被修改，请刷新后重试"),
            ErModelError::InvalidDefinition(reason) => write!(f, "关系定义无效：{reason}"),
            ErModelError::DuplicateRole => write!(f, "同一对表之间已存在同名角色"),
        }
    }
}

/// 校验关系定义的结构性约束（§5.3）：
/// - column_pairs 非空、同 pair 内左右列都非空、无重复。
/// - required_filters 列非空、op 已知（由类型保证）、字面量是字面量（由 ErLiteral 类型保证）。
/// 端点/列在当前快照是否存在属绑定校验，由宿主做（服务不持有结构快照）。
fn validate_definition(rel: &ErRelationship) -> Result<(), ErModelError> {
    if rel.column_pairs.is_empty() {
        return Err(ErModelError::InvalidDefinition("字段配对不能为空".into()));
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in &rel.column_pairs {
        if p.left_column.is_empty() || p.right_column.is_empty() {
            return Err(ErModelError::InvalidDefinition("字段配对含空列".into()));
        }
        if !seen.insert((&p.left_column, &p.right_column)) {
            return Err(ErModelError::InvalidDefinition("字段配对重复".into()));
        }
    }
    for f in &rel.required_filters {
        if f.column_id.is_empty() {
            return Err(ErModelError::InvalidDefinition("过滤条件引用空列".into()));
        }
        if matches!(
            f.op,
            fluxdb_core::ErFilterOp::IsNull | fluxdb_core::ErFilterOp::IsNotNull
        ) && !matches!(f.literal, fluxdb_core::ErLiteral::Null)
        {
            return Err(ErModelError::InvalidDefinition(
                "为空/不为空条件不能携带字面量".into(),
            ));
        }
        // op 是枚举，不可能是任意 SQL 字符串；literal 是 ErLiteral（结构化字面量），
        // 不接受表达式/函数/子查询——类型系统已保证不拼任意 SQL。
        let _ = &f.op;
        let _ = &f.literal;
    }
    Ok(())
}

/// 生产用关系存储适配：把 fluxdb-storage 的 FileStorage 接到 `ErRelationshipStore`。
/// 单测用内存 MemStore（见下），生产用本适配器经 sqlite kv 持久化（§5/§7）。
pub struct FileErRelationshipStore {
    storage: fluxdb_storage::FileStorage,
    scope_key: String,
}

impl FileErRelationshipStore {
    pub fn new(storage: fluxdb_storage::FileStorage, scope_key: impl Into<String>) -> Self {
        Self {
            storage,
            scope_key: scope_key.into(),
        }
    }
}

impl ErRelationshipStore for FileErRelationshipStore {
    fn load(&self, _scope: &str) -> fluxdb_core::Result<Vec<ErRelationship>> {
        self.storage.load_er_relationships(&self.scope_key)
    }
    fn save(&self, _scope: &str, rels: &[ErRelationship]) -> fluxdb_core::Result<()> {
        self.storage.save_er_relationships(&self.scope_key, rels)
    }
}

/// ER 逻辑关系服务（给定 scope 的目录操作）。构造后持有一个可变目录。
///
/// 并发：目录读改写（load→mutate→save）在 `lock` 内整体串行，同进程内多操作不会互相
/// 覆盖（§7：delete/update/confirm 等都不能绕过既有并发保护）。跨进程（多窗口）不在本文
/// 范围（桌面单实例）：`ponytail: session 内 Mutex 串行写，跨进程需 storage 层 CAS 再做`。
pub struct ErModelService {
    scope_key: String,
    store: Box<dyn ErRelationshipStore>,
    lock: std::sync::Mutex<()>,
}

impl ErModelService {
    pub fn new(scope_key: impl Into<String>, store: Box<dyn ErRelationshipStore>) -> Self {
        Self {
            scope_key: scope_key.into(),
            store,
            lock: std::sync::Mutex::new(()),
        }
    }

    fn load_all(&self) -> Result<Vec<ErRelationship>, ErModelError> {
        self.store
            .load(&self.scope_key)
            .map_err(|error| ErModelError::Storage(error.to_string()))
    }

    fn save_all(&self, rels: &[ErRelationship]) -> fluxdb_core::Result<()> {
        self.store.save(&self.scope_key, rels)
    }

    /// 列出当前目录全部关系（含 proposed/rejected；usage 需另查）。
    pub fn list(&self) -> Result<Vec<ErRelationship>, ErModelError> {
        self.load_all()
    }

    /// 新建（proposed 或按 origin 置 review）。带结构校验 + role 唯一（§5.3）。
    pub fn create(&self, mut rel: ErRelationship) -> Result<ErRelationship, ErModelError> {
        let _guard = self.lock.lock().unwrap();
        validate_definition(&rel)?;
        let mut all = self.load_all()?;
        // role 在 (left,right) 同一对实体内有向唯一（§5.3）；自关联两个方向也算同一对。
        let dup = all.iter().any(|r| {
            let same_pair = (r.left_entity == rel.left_entity && r.right_entity == rel.right_entity)
                || (r.left_entity == rel.right_entity && r.right_entity == rel.left_entity);
            same_pair && r.role == rel.role
        });
        if dup {
            return Err(ErModelError::DuplicateRole);
        }
        if rel.revision == 0 {
            rel.revision = 1;
        }
        if rel.review.state == ErReviewState::Confirmed && rel.review.confirmed_revision.is_none() {
            rel.review.confirmed_revision = Some(rel.revision);
        }
        all.push(rel.clone());
        self.save_all(&all)
            .map_err(|_| ErModelError::InvalidDefinition("保存失败".into()))?;
        Ok(rel)
    }

    /// 原子采纳一批由宿主校验过的 Agent 建议。
    ///
    /// 用户已在逐条审阅弹框中明确选择，因此这里直接写入 `Confirmed`，不再留下
    /// `Proposed` 中间态。模型不能调用此方法；review/identity/revision 均由宿主重写，
    /// 且任一条冲突都会整批失败，避免部分采纳后用户无法判断批内剩余状态。
    pub fn create_confirmed_batch(
        &self,
        mut relationships: Vec<ErRelationship>,
        by: &str,
    ) -> Result<Vec<ErRelationship>, ErModelError> {
        if relationships.is_empty() {
            return Err(ErModelError::InvalidDefinition("没有可采纳的关系".into()));
        }
        if relationships.len() > 50 {
            return Err(ErModelError::InvalidDefinition(
                "单次最多采纳 50 条关系".into(),
            ));
        }
        if by.trim().is_empty() {
            return Err(ErModelError::InvalidDefinition(
                "确认人不能为空".into(),
            ));
        }
        let _guard = self.lock.lock().unwrap();
        let mut all = self.load_all()?;
        let mut identity_keys = std::collections::BTreeSet::new();
        for index in 0..relationships.len() {
            let (left_entity, right_entity, role) = {
                let rel = &relationships[index];
                (
                    rel.left_entity.clone(),
                    rel.right_entity.clone(),
                    rel.role.clone(),
                )
            };
            let rel = &relationships[index];
            if rel.origin != fluxdb_core::ErRelationshipOrigin::Agent
                || rel.enforcement.kind != fluxdb_core::ErEnforcementKind::None
                || rel.enforcement.constraint_ref.is_some()
                || rel.enforcement.enforced.is_some()
                || rel.validity.state != fluxdb_core::ErValidityState::Current
            {
                return Err(ErModelError::InvalidDefinition(
                    "AI 建议只能由宿主按 Agent/无约束/当前结构写入".into(),
                ));
            }
            validate_definition(rel)?;
            let key = fluxdb_core::er_relationship_identity_key(rel);
            if !identity_keys.insert(key.clone())
                || all.iter().any(|current| {
                    fluxdb_core::er_relationship_identity_key(current) == key
                })
            {
                return Err(ErModelError::InvalidDefinition(
                    "关系已存在或批次内重复".into(),
                ));
            }
            let duplicate_role = all.iter().chain(relationships[..index].iter()).any(|current| {
                ((current.left_entity == left_entity && current.right_entity == right_entity)
                    || (current.left_entity == right_entity
                        && current.right_entity == left_entity))
                    && current.role == role
            });
            if duplicate_role {
                return Err(ErModelError::DuplicateRole);
            }
            let rel = &mut relationships[index];
            rel.id = format!("agent-{}", uuid::Uuid::new_v4());
            rel.revision = 1;
            rel.review = ErRelationshipReview {
                state: ErReviewState::Confirmed,
                confirmed_revision: Some(1),
                confirmed_by: Some(by.to_string()),
            };
        }
        all.extend(relationships.iter().cloned());
        self.save_all(&all)
            .map_err(|_| ErModelError::InvalidDefinition("保存失败".into()))?;
        Ok(relationships)
    }

    /// 按 id 找（可变借用以构造 next revision）。
    fn index_of(all: &[ErRelationship], id: &str) -> Option<usize> {
        all.iter().position(|r| r.id == id)
    }

    /// 更新：expected_revision 不匹配 → RevisionConflict（§6.3 乐观锁）；闭包返回新定义。
    fn update_inner(
        &self,
        id: &str,
        expected_revision: u64,
        f: impl FnOnce(&mut ErRelationship) -> Result<(), ErModelError>,
    ) -> Result<ErRelationship, ErModelError> {
        let _guard = self.lock.lock().unwrap();
        let mut all = self.load_all()?;
        let i = Self::index_of(&all, id).ok_or(ErModelError::NotFound)?;
        if all[i].revision != expected_revision {
            return Err(ErModelError::RevisionConflict);
        }
        f(&mut all[i])?;
        validate_definition(&all[i])?;
        let duplicate_role = all.iter().enumerate().any(|(index, other)| {
            index != i
                && ((other.left_entity == all[i].left_entity
                    && other.right_entity == all[i].right_entity)
                    || (other.left_entity == all[i].right_entity
                        && other.right_entity == all[i].left_entity))
                && other.role == all[i].role
        });
        if duplicate_role {
            return Err(ErModelError::DuplicateRole);
        }
        all[i].revision += 1;
        let out = all[i].clone();
        self.save_all(&all)
            .map_err(|_| ErModelError::InvalidDefinition("保存失败".into()))?;
        Ok(out)
    }

    /// 更新定义（字段配对/端点/基数/过滤）：生成新修订并使旧确认失效（§5.2/§6.3）。
    /// 确认/拒绝走 `confirm`/`reject`（不经过此失效，因为正在设置确认状态本身）。
    pub fn update(
        &self,
        id: &str,
        expected_revision: u64,
        patch: impl FnOnce(&mut ErRelationship) -> Result<(), ErModelError>,
    ) -> Result<ErRelationship, ErModelError> {
        self.update_inner(id, expected_revision, |r| {
            let was_confirmed = r.review.state == ErReviewState::Confirmed;
            patch(r)?;
            if was_confirmed {
                // 定义被编辑后，旧确认不再代表当前状态：回到 proposed，需重新确认（§5.2）。
                // 只清 confirmed_revision 而仍留 Confirmed 会让 usage 的「确认过期」检查
                // 因 None 走不到 outdated 分支而误判为可候选，故必须降级回 proposed。
                r.review.state = ErReviewState::Proposed;
                r.review.confirmed_revision = None;
                r.review.confirmed_by = None;
            }
            Ok(())
        })
    }

    /// 同对象改名/列重命名的机械重绑：一个事务内更新身份和确认修订。
    /// 只允许端点与列身份变化，角色、谓词、基数、来源和审核状态等语义字段必须原样。
    pub fn rebind_identity(
        &self,
        id: &str,
        expected_revision: u64,
        mut replacement: ErRelationship,
    ) -> Result<ErRelationship, ErModelError> {
        self.update_inner(id, expected_revision, |current| {
            if replacement.id != current.id
                || replacement.role != current.role
                || replacement.match_cardinality != current.match_cardinality
                || replacement.origin != current.origin
                || replacement.enforcement != current.enforcement
                || replacement.validity != current.validity
                || replacement.description != current.description
                || replacement.evidence_refs != current.evidence_refs
                || replacement.column_pairs.len() != current.column_pairs.len()
                || replacement.required_filters.len() != current.required_filters.len()
                || replacement.required_filters.iter().zip(&current.required_filters)
                    .any(|(new, old)| new.side != old.side || new.op != old.op || new.literal != old.literal)
            {
                return Err(ErModelError::InvalidDefinition("自动重绑不得更改关系语义".into()));
            }
            replacement.revision = current.revision;
            replacement.review = current.review.clone();
            *current = replacement;
            if current.review.state == ErReviewState::Confirmed {
                current.review.confirmed_revision = Some(current.revision + 1);
            }
            Ok(())
        })
    }

    /// 人工确认（§6.3）：标记 confirmed 并记录当前 revision。
    pub fn confirm(
        &self,
        id: &str,
        expected_revision: u64,
        by: &str,
    ) -> Result<ErRelationship, ErModelError> {
        self.update_inner(id, expected_revision, |r| {
            if r.validity.state != fluxdb_core::ErValidityState::Current {
                return Err(ErModelError::InvalidDefinition(
                    "关系结构尚未解析，请修复端点与字段后再确认".into(),
                ));
            }
            r.review = ErRelationshipReview {
                state: ErReviewState::Confirmed,
                confirmed_revision: Some(r.revision + 1),
                confirmed_by: Some(by.to_string()),
            };
            Ok(())
        })
    }

    /// 拒绝（不删除，保留定义与证据，§5.2）。
    pub fn reject(
        &self,
        id: &str,
        expected_revision: u64,
    ) -> Result<ErRelationship, ErModelError> {
        self.update_inner(id, expected_revision, |r| {
            r.review.state = ErReviewState::Rejected;
            Ok(())
        })
    }

    /// 删除本地逻辑关系（§7 删除语义）：只删本地模型，不执行 DDL、不动数据库物理外键。
    /// 与 update/confirm 一致采用 expected_revision 修订校验（不能绕过并发保护）；
    /// load→remove→save 在 `lock` 内原子执行，写入失败返回错误、不假装删除成功。
    pub fn delete(&self, id: &str, expected_revision: u64) -> Result<(), ErModelError> {
        let _guard = self.lock.lock().unwrap();
        let mut all = self.load_all()?;
        let i = Self::index_of(&all, id).ok_or(ErModelError::NotFound)?;
        if all[i].revision != expected_revision {
            return Err(ErModelError::RevisionConflict);
        }
        all.remove(i);
        self.save_all(&all)
            .map_err(|_| ErModelError::InvalidDefinition("保存失败".into()))?;
        Ok(())
    }

    /// 计算某关系的 usage（§5.9）；`ctx` 由宿主从结构快照构造。
    pub fn usage(&self, id: &str, ctx: &ErUsageContext) -> Result<ErUsage, ErModelError> {
        let all = self.load_all()?;
        let i = Self::index_of(&all, id).ok_or(ErModelError::NotFound)?;
        Ok(all[i].usage(ctx))
    }

    /// 生成全部可用关系的 JOIN 计划（§6.6/查询地基）：仅 `join_candidate` 的关系入列，
    /// 每条完整保留复合配对与 required_filters。`ctx` 由宿主按关系从结构快照构造；
    /// coverage 不完整的关系不产出（宿主以 coverage 表达，不解释为“无关系”）。
    pub fn join_plans(
        &self,
        mut ctx: impl FnMut(&ErRelationship) -> ErUsageContext,
    ) -> Vec<fluxdb_core::ErJoinPlan> {
        self.load_all()
            .unwrap_or_default()
            .iter()
            .filter_map(|r| r.join_plan(&ctx(r)))
            .collect()
    }

    /// 结构刷新后的重绑报告（§5.2）：按旧快照映射 `old_entities_by_id` + 新快照 `new_entities`
    /// 判定每条关系左右端实体/列是否可重绑；不自动改关系（unresolved 保留定义与证据，
    /// 由 UI 集中列出待处理项，用户手工重绑）。返回每条关系两端的判定。
    pub fn rebind_report(
        &self,
        old_entities_by_id: &std::collections::HashMap<String, fluxdb_core::ErRebindEntity>,
        new_entities: &[fluxdb_core::ErRebindEntity],
    ) -> Vec<(String, Vec<ErRebindEndpointStatus>)> {
        self.load_all()
            .unwrap_or_default()
            .iter()
            .map(|r| {
                let mut endpoints = Vec::new();
                // 左端。
                if let Some(old) = old_entities_by_id.get(&r.left_entity) {
                    let used = rel_columns_on(r, fluxdb_core::ErRelationSide::Left);
                    endpoints.push(rebase_endpoint(old, &used, new_entities));
                }
                // 右端。
                if let Some(old) = old_entities_by_id.get(&r.right_entity) {
                    let used = rel_columns_on(r, fluxdb_core::ErRelationSide::Right);
                    endpoints.push(rebase_endpoint(old, &used, new_entities));
                }
                (r.id.clone(), endpoints)
            })
            .collect()
    }
}

/// 单个端点重绑状态（供 UI「待处理项」列出）。
#[derive(Clone, Debug, PartialEq)]
pub struct ErRebindEndpointStatus {
    pub old_entity_id: String,
    pub matched_entity: Option<String>,
    /// 删除重建（stable 变但名同）→ 需确认。
    pub needs_review: bool,
    /// 实体未找到。
    pub entity_unresolved: bool,
    /// 缺失（unresolved）的列（记录具体 column_id，供手工重绑；绝不静默接相似列）。
    pub unresolved_columns: Vec<String>,
}

/// 取关系某侧用到的列（column_pairs 中该侧列 + required_filters 该侧列），去重保序。
fn rel_columns_on(r: &ErRelationship, side: fluxdb_core::ErRelationSide) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in &r.column_pairs {
        let col = match side {
            fluxdb_core::ErRelationSide::Left => &p.left_column,
            fluxdb_core::ErRelationSide::Right => &p.right_column,
        };
        if !out.contains(col) {
            out.push(col.clone());
        }
    }
    for f in &r.required_filters {
        if f.side == side && !out.contains(&f.column_id) {
            out.push(f.column_id.clone());
        }
    }
    out
}

/// 单个端点重绑（§5.2）→ 状态。
fn rebase_endpoint(
    old: &fluxdb_core::ErRebindEntity,
    used: &[String],
    new_entities: &[fluxdb_core::ErRebindEntity],
) -> ErRebindEndpointStatus {
    let out = fluxdb_core::rebind_entity(old, used, new_entities);
    ErRebindEndpointStatus {
        old_entity_id: old.entity_id.clone(),
        matched_entity: out.matched_entity,
        needs_review: out.entity_needs_review,
        entity_unresolved: out.entity_unresolved,
        unresolved_columns: out
            .columns
            .iter()
            .filter(|c| c.unresolved)
            .map(|c| c.old_column_id.clone())
            .collect(),
    }
}

/// 由结构化表身份生成实体稳定 ID。与 core `er_entity_id` 同规则（关系端点/列引用的
/// 唯一生成规则），避免桌面 ER、应用层查询与结构重绑各算一套身份。
fn er_snapshot_entity_id(reference: &fluxdb_core::ErTableRef) -> String {
    fluxdb_core::er_entity_id(reference)
}

/// 由列身份生成列 ID（`实体ID::列名`）。
fn er_snapshot_column_id(reference: &fluxdb_core::ErTableRef, column: &str) -> String {
    fluxdb_core::er_column_id(reference, column)
}

/// 由已加载的表节点（含字段，如字段按需加载后的 `ErTableNode`）构建结构快照
/// （§5.2/D1「结构快照解析」）：实体限定名 + 列名，无稳定对象标识（连接器未暴露时如实留空，
/// 不伪造稳定身份，重绑走 §5.2 第 2/3/4 步：限定名→列名→unresolved）。
/// 跨 schema 同名、含点标识符都按 `ErTableRef` 结构化身份生成稳定 entity_id/column_id。
pub fn er_snapshot_from_tables(tables: &[fluxdb_core::ErTableNode]) -> Vec<fluxdb_core::ErRebindEntity> {
    tables
        .iter()
        .filter(|t| t.status != fluxdb_core::ErLoadStatus::NotLoaded)
        .map(|t| fluxdb_core::ErRebindEntity {
            entity_id: er_snapshot_entity_id(&t.reference),
            qualified_name: t.reference.display(),
            // PG 表稳定对象标识（pg_class.oid）；无公开稳定号时为 None（走限定名重绑）。
            stable_id: t.stable,
            columns: t
                .columns
                .iter()
                .map(|c| fluxdb_core::ErRebindColumn {
                    column_id: er_snapshot_column_id(&t.reference, &c.name),
                    name: c.name.clone(),
                    stable_id: c.stable,
                })
                .collect(),
        })
        .collect()
}


