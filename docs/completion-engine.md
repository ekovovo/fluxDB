# SQL 补全引擎（fluxdb-completion）

面向「IDE 级语义补全」的独立补全包。设计参考 DBeaver 新引擎（AST + 符号表 + 光标槽位）
与 DataGrip 行为（ON 槽位硬限定、语义 kind 排序）。

## 边界

- 只依赖 `sqlparser`（含 visitor），**不依赖任何 workspace crate**；宿主反向实现 trait。
- 新增数据库 = 实现 `CompletionDialect` + 提供 `MetadataSource`，引擎本身不改。
- 不负责 UI、缓存、网络与触发时机；这些留在 `fluxdb-app` / desktop 层。

## 两个 trait

```rust
pub trait CompletionDialect {
    fn parser_dialect(&self) -> &'static dyn Dialect;
    fn keywords(&self) -> &'static [&'static str];
    fn functions(&self) -> &'static [&'static str];
    fn quote_ident(&self, name: &str) -> String;
    fn qualify_bare_table(&self) -> bool; // 达梦需要 schema 限定
}

pub trait MetadataSource {
    fn tables(&self, schema: Option<&str>) -> Vec<TableMeta>;
    fn schemas(&self) -> Vec<String>;
    fn columns(&self, key: &TableKey) -> Vec<ColumnMeta>;
    fn foreign_keys(&self, key: &TableKey) -> Vec<ForeignKeyMeta>;
    fn routines(&self, schema: Option<&str>) -> Vec<RoutineMeta> { Vec::new() }
}
```

## 处理链路

1. **词法**（`word.rs`）：光标词边界、限定符链（`schema.table.col`）、引号标识符、前序关键字；按分号切语句。
2. **容错解析**（`parse.rs`）四级阶梯：
   1. 掩码光标残词为等长占位符（必须优先，避免 `u. FROM` 被贪婪解析成 `u.FROM AS users`）；
   2. 原文解析；
   3. 在 token 间隙插入占位符（偏移回映射）；
   4. 从尾部按 token 边界截断（最多 12 次）。
   全败 → 词法兜底。
3. **符号表**（`symbols.rs`）：关系/别名、CTE（无显式列时从投影推导）、派生表、投影别名。
   Phase 1–3 为整语句平铺收集，子查询可见外层别名。
4. **槽位定位**（`locate.rs`）：取包含光标的最小 AST 节点映射为 `ClauseSlot`。
5. **槽位策略**（`strategy.rs`）：每个槽位决定候选来源与基础分。
6. **过滤排序**（`engine.rs`）：引号标识符前缀匹配、其余 ASCII 不敏感子序列；按
   `分数 → 前缀命中 → 标签` 排序，最后按 label/kind/schema 去重。

## 槽位与评分

| 槽位 | 候选 |
| --- | --- |
| `StatementStart` | 语句起始关键字 |
| `Expression{SelectItem/Where/Having/GroupBy/OrderBy/JoinOn}` | 关系列 + 函数（无限定符）；ORDER/GROUP 额外给投影别名；谓词给 NULL/TRUE/FALSE |
| `FromTable` | 表/视图/schema + 续接关键字；JOIN 时外键目标表加分 |
| `TableAlias` | 续接关键字 + 外键推导的 `ON l.c = r.fk` snippet |
| `JoinOnLeft` | 有外键时只给 FK 关联列，否则两侧全列 |
| `JoinOnRight` | 左侧列在整条 JOIN 链中的配对列；无外键回退目标表全列 |
| `InsertColumn` / `UpdateAssignLhs` | 表列，排除已写列 |
| `UpdateAssignValue` | DEFAULT/NULL/TRUE/FALSE + 函数 |

基础分：FK 列 120 / ON snippet 110 / 列 100 / 投影别名 95 / 关联表 95 / 表 90 / schema 85 /
函数 60 / 关键字 50。

## 有意变更（相对旧启发式）

- 限定符既非别名、表名也非 CTE/派生表 → 空候选，不再兜底全列。
- `ON` 条件由外键硬限定，只有缺少 FK 元数据时才回退全列。
- 未完成的 `INSERT (col,` / `UPDATE SET` 由前缀词法识别（sqlparser 需要完整语句）。
- 同分内只做个性化重排（采纳历史），不跨语义分层调序。
- 同名表跨 schema 时自动加 namespace 前缀，避免点错对象。
- 达梦裸表补全的插入文本带 schema 限定；默认 schema 有对象时仍并入对象树已展开的
  其他 schema（引擎负责前缀过滤）。
- 补全不再单独产出触发器候选（触发器不是合法表达式；旧链路的 Trigger 候选已移除）。
- 关键字按子句收敛：空前缀只给当前子句相关的一小撮（投影 10 / 谓词 22 / ORDER BY 6），
  打字中才放开全量词表靠前缀收窄。此前任何表达式位置都灌 60+ 个 DDL 关键字。
- `FROM a, |` / `FROM a, b|` 回到表名槽（旧判定按「最近一个词」一律当别名位置，出不了表名）。
- 编辑器语义诊断（未知表/列/函数）与函数参数提示已随旧链路删除；语法诊断保留。

## 宿主接线（fluxdb-app）

`crates/fluxdb-app/src/parts/completion_engine.rs` 实现 `MetadataSource`：

- 表/列/例程/外键全部走既有 `indexed_*_with_cancel` 系列与 CompletionIndex 缓存。
- 列与外键在一次请求内按 `TableKey` 缓存；外键沿用既有 300s TTL 缓存。
- 裸表名在默认桶未命中时按表名回查所属 schema。
- `Candidate` 直转 `QueryCompletionItem`；Schema 插入 `name.`，Snippet 显式标记格式。
- 引号规则与个性化同分重排留在宿主（与旧行为一致）。

## 分期状态

- 第 1 期：骨架、容错解析、槽位定位、FROM/SELECT 列 —— 完成
- 第 2 期：JOIN 全链路、表达式槽位、投影别名、ON snippet —— 完成
- 第 3 期：INSERT/UPDATE、CTE/派生表、词法 DML 降级 —— 完成
- 第 4 期：`fluxdb-app` 接线并直接切换（无 legacy 开关）—— 完成
- 第 5 期：删除旧补全代码 —— 完成。`crates/fluxdb-app/src/parts/query_completion.rs`
  由 1396 行缩到 255 行，只剩仍有跨模块消费方的纯工具（`quote_identifier` /
  `identifier_needs_quote` / `sql_identifier_tokens` / `sql_ddl_impact` / `now_ts` /
  `RecencyFrequency`）；desktop 的语义诊断与签名提示一并拆除

验证：`cargo test -p fluxdb-completion`（75 例）、`cargo test -p fluxdb-app`（57 例，另有
3 个默认 ignore 的真库冒烟）、`cargo clippy --locked -p fluxdb-completion --all-targets`。

真库冒烟（本地容器，密码只从环境变量读取）：

```bash
FLUXDB_SMOKE_MYSQL_PASSWORD=... cargo test -p fluxdb-app mysql_live_metadata_feeds_completion -- --ignored
FLUXDB_SMOKE_DAMENG_PASSWORD=... cargo test -p fluxdb-app dameng_live_metadata_feeds_completion -- --ignored
FLUXDB_SMOKE_POSTGRES_PASSWORD=... cargo test -p fluxdb-app postgres_live_metadata_feeds_completion -- --ignored
```

覆盖：表补全、限定符列补全、MySQL/PostgreSQL 外键 ON 硬限定（`orders.customer_id`）。
