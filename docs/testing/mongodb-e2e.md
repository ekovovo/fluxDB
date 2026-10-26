# MongoDB 测试

> 关联实现说明：[MongoDB 支持](../mongodb.md)
>
> 通用测试规范见 [Rust 风格与单测](rust-style-and-unit-tests.md)。

## 1. 分层

| 层 | 位置 | 是否需要真实 MongoDB |
| --- | --- | --- |
| URI 解析 / 档案往返 | `crates/fluxdb-core/src/parts/mongo_profile.rs` 内 `#[cfg(test)]` | 否 |
| 错误文本脱敏、Extended JSON | `crates/fluxdb-connectors/src/parts/mongo/client.rs` 内 `mongo_wire_tests` | 否 |
| 导入：格式嗅探 / 类型推断 / 单元格转换 / JSON 错误定位 / 非 ASCII 与宽表预览 | `crates/fluxdb-connectors/src/parts/mongo/import.rs` 内 `mongo_import_engine_tests`（8 个用例） | 否 |
| 刷新替换语义 | `crates/fluxdb-app/src/parts/table_info.rs` 内 `replace_loaded_children_tests` | 否 |
| 连接器端到端 | `crates/fluxdb-connectors/tests/mongo_live.rs` | 是 |
| UI 端到端（生产视图） | `apps/fluxdb-desktop/src/main_parts/mongo_sidebar_tests.rs` | 是 |

## 2. 凭据注入

真实用例只从 `FLUXDB_MONGO_URI` 读连接串，未设置时**自动跳过**（打印 `跳过：未设置 FLUXDB_MONGO_URI`，用例仍算通过）。

凭据由调用方从自己的凭据文件注入，**不得写进仓库、测试夹具或日志**：

```bash
set -a; . /path/to/atlas-credentials.env; set +a
export FLUXDB_MONGO_URI="$MONGODB_URI"
export FLUXDB_MONGO_DB=fluxdb_demo
```

目标库解析顺序：URI 默认库 → `FLUXDB_MONGO_DB` → `fluxdb_demo`。

## 3. 连接器用例（`crates/fluxdb-connectors/tests/mongo_live.rs`）

```bash
cargo test --locked -p fluxdb-connectors --test mongo_live -- --test-threads=1
```

| 用例 | 覆盖 |
| --- | --- |
| `mongo_live_ping_create_query_metadata_and_cleanup` | 认证 → 建集合 → 插入（ObjectId/日期/嵌套）→ 查询 → 条件查询 → 投影 → 取消标志 → **整篇替换并复查** → **拒绝改 `_id`** → 集合信息 → 重复创建幂等 → **删除文档并复查（含重复删除必须失败）** → **新增文档（按返回 `_id` 复查、Decimal128 类型保留、自带 ObjectId 必须原样采用、非 ObjectId 的 `_id` 必须拒绝）** → 删集合 → 列表校验 |
| `mongo_live_aggregate_and_indexes` | 聚合分组求和、默认 `_id` 索引 |
| `mongo_live_server_info_reports_real_topology` | `buildInfo.version`、拓扑判定、`listDatabases` |
| `mongo_live_import_documents_from_csv_and_json` | 导入链路：CSV 列类型推断（`_id`→objectId、int32/double/boolean/date）→ 重复 `_id` 只失败一条（`ordered:false`）→ 按 `$oid` 复查确认主键是 ObjectId → JSON 数组（`$oid`/`$date` 还原）→ JSONL（一行坏 JSON 跳过，错误带行号） |
| `mongo_live_operations_survive_small_caller_stack` | **栈回归**：192 KiB 栈线程里跑建连 + 集合列表 + `find` 游标解码 |

每个用例自建 `fluxdb_smoke_*` 集合并在结束时删除，不触碰既有数据。

### 3.1 栈回归用例的意义

`Runtime::block_on` 在调用方线程 poll future，gpui 后台任务在 macOS 上是约 512 KiB 栈的 GCD worker。曾经在 `listCollections` 序列化时溢栈并 `abort()`（SIGILL / SIGABRT，不可捕获）。

该用例用 192 KiB 栈线程复现同样环境：把 `mongo_run` / `mongo_block_on` 换成直接 `mongo_runtime().block_on`，测试进程会直接崩（`fatal runtime error: stack overflow, aborting`），而不是断言失败。恢复放大栈后稳定通过。

## 4. UI 端到端（`apps/fluxdb-desktop/src/main_parts/mongo_sidebar_tests.rs`）

```bash
cargo test --locked -p fluxdb-desktop --bin fluxdb-desktop \
  mongo_database_expands_and_collection_opens_workspace
```

走**真实生产视图**（`navicat_main_new` + `Root`），全部通过无头事件驱动，不直接调处理函数：

1. 双击 `connection-row:{id}` → 等 `database-row:{db}` 出现（真实建连）
2. 点击库行 → 等 `object-group-row:collections:{db}` 出现（懒加载集合列表）
3. 点击分组行 → 等集合行 `table-tree-row:{name}` 出现
4. 点击集合行 → 等 `mongo-collection-page:{db}/{collection}` 出现
5. 等标签页 `MongoCollectionState.page` 有真实返回文档
6. 等 `mongo-document-fields:0` 出现（字段列表，不是整段 JSON）→ 点 `mongo-document-toggle:0`（`mongo-document-toggle:{下标}` 卡片折叠箭头）→ 选择器消失 → 再点恢复
	- 整条卡头可点：再点 `mongo-document-head:0`（卡头本体，不是箭头）同样能收起/展开
7. 点 `mongo-display-tab:Json` → 等 `mongo-document-json:0` 出现，且卡片头 `mongo-document-toggle:0` 仍在、`mongo-document-fields:0` 消失（JSON 是逐条卡片正文，不是整页数组）
	- 默认折叠：标签页的 `mongo_json_fold_toggles` 必须为空（空 = 根展开、嵌套全收起的默认态），数出此时可见行数
	- 展开：从第 1 行起找窗口内的 `mongo-json-fold:0:{行}`（该选择器只出现在可折叠节点行；普通行是 `mongo-json-fold:0-plain:{行}`，注意 `-plain` 在前缀之后），点击 → 等可见行数增加
	- 点 `mongo-display-tab:Documents` 还原
8. 点 `mongo-display-tab:Table` → 等 `mongo-document-table` 出现，且首行 `mongo-table-cell:0:id` 渲染、`mongo-document-fields:0` 消失；首行的 `mongo-document-delete:0` 与 `mongo-document-copy:0` 必须渲染且中心点在窗口内（固定操作列）→ 点复制按钮，等 `app-message-overlay` 出现（剪贴板写入必须有可观察反馈）→ 切回文档视图
9. 从真实返回的文档里挑一个落在窗口内的对象/数组字段 → 断言 `mongo-field-children:{_id}/{字段}` 默认不存在 → 点摘要行后出现
10. 删除文档：先插入一篇固定 `_id`（`fluxdb-e2e-delete`）的探针文档并重查 → 收起展开的卡片 → 点该行 `mongo-document-delete:{下标}` → 等 `mongo-document-delete-modal` / `mongo-document-delete-confirm` 出现，并断言此时探针文档仍在（没点确认不能删）→ 点 `mongo-document-delete-cancel`：确认框消失且文档仍在 → 再点删除并确认 → 等本页不再含该 `_id`。用例只删自己插入的探针，绝不删演示数据。
10.1. 复制文档：插入固定 `name` 的探针文档 → 点该行 `mongo-document-duplicate:{下标}` → 等 `mongo-document-duplicate-drawer`（与编辑共用的右侧抽屉）出现，断言初值**不含 `_id`** → 点 `mongo-document-editor-cancel`：抽屉消失且不写入 → 再点复制并 `mongo-document-duplicate-confirm` → 等本页出现两条同名文档，且其中一条 `id` 不等于源 `_id`（副本 `_id` 由服务端生成）
10.2. 新增文档：点结果条 `mongo-results-insert`（先断言它落在窗口内，越界点击会被静默丢弃）→ 等 `mongo-document-insert-drawer` 出现，断言初值只有 `_id` 一个字段且是 `$oid` 包装 → 把草稿补成「同一个 `_id` + 一个探针 `name`」→ 点 `mongo-document-insert-confirm` → 等本页出现 `id` **等于预填 `_id`** 的文档（预填的 id 必须真的落库，不能只是界面假象）→ 断言抽屉已关闭
10.3. 导入文档：点结果条 `mongo-results-import`（先断言落在窗口内）→ 等 `mongo-import-modal` 出现 → 断言此时 `pending_mongo_import.source/preview` 都为空（未选文件不出预览）→ 点 `mongo-import-close` → 等弹窗消失。
	- 选文件走系统原生对话框（`prompt_for_paths`），无头环境无法驱动，因此本步只验证入口、弹窗与关闭；解析与写入链路由连接器用例 `mongo_live_import_documents_from_csv_and_json` 覆盖，不在这里假装验证。
10.4. 创建集合后自动刷新：库行右键（`MouseDownEvent` + `MouseButton::Right`）→ 等 `database-menu-item:Create collection` → 点开 `mongo-create-collection-modal` → 在 `mongo-create-collection-input` 输入探针集合名 → 点 `mongo-create-collection-confirm` → **不点刷新**，直接等 `table-tree-row:{集合}` 出现。这是「创建集合后没刷新该库」的回归点：控制器只替换第一层库列表，界面必须自己重取该库子节点，只标记「已加载」会把过期快照当成最新。结束后用 `MongoRunCommand` 发 `{"drop": …}` 清理探针集合。
11. 文档编辑抽屉：断言 `mongo-document-editor` 初始不存在 → 先临时插入一篇固定 `_id`（`fluxdb-e2e-switch`）的探针文档并重查（演示库集合可能只剩一篇，未保存切换需要两张卡片），把展开着的卡片逐个收起让卡头都落进窗口 → 点卡片头 `mongo-document-detail:0` → 等抽屉出现
	- 断言编辑框初值等于会话基线 `base_json`（未改动时保存按钮禁用）
	- 取本页首篇文档，把第一个顶层标量字段改掉（字符串追加 `[fluxdb-e2e]`，没有字符串才退回数值 +1；`_id` 一律不动），通过真实编辑框 `set_value` 替换全文（不直接调保存函数）
	- 未保存切换：点另一张卡片的编辑按钮 → 等 `mongo-document-discard` 出现 → 点 `mongo-document-discard-cancel` → 确认框消失且草稿仍是改后的文本
	- 放弃并切换：再点另一张卡片 → 点 `mongo-document-discard-confirm` → 编辑会话切到目标文档（`editor.index` 为目标下标）
	- 再点回首篇、写入同一份草稿 → 点 `mongo-document-editor-save` → 等抽屉消失 → 等本页同 `_id` 文档的 JSON 与期望语义相等（重新查询带回新值）
	- 还原：用同一条 `SaveMongoDocument` 命令写回原文档并重查，断言已回到原值；随后删除探针文档并重查确认消失——用例只借真实库验证写回，不留改动；第 12 步按 `_id` 清 switch 探针、按 `name` 清复制探针（副本 `_id` 由服务端生成，只能按内容定位）

卡片 / 字段 / 表格行的纯逻辑（BSON 包装判类型、`ObjectId("…")` 文本、卡片身份取 `_id`、表格列序与类型、JSON 折叠键按文档隔离）另有 `mongo_documents_tests` 五个单测、`mongo_document_delete_tests` 两个删除提示单测、`mongo_document_duplicate_tests` 五个复制草稿单测（剥 `_id`、字符串 `_id`、**字段顺序不变**、BSON 类型保留、非法输入透传）、`mongo_document_insert_tests` 三个新增草稿单测（`_id` 是 24 位小写十六进制、同进程不重复、初值只有 `$oid` 包装的 `_id`）覆盖，不需要连库。

断言既有可观察反馈（debug 选择器边界），也有业务结果（返回条数 > 0）。第 9–11 步都要先比对目标中心点是否在窗口内——越界点击会被 gpui 静默丢弃；卡片正文很高，所以要先把展开的卡片收起，否则第二张卡片只有卡头露出视口外。第 11 步的保存由真实按钮触发，但替换全文走编辑器实体（`set_value` 不产生 Change 事件，按钮可用性由编辑框的 Change 订阅驱动重绘）；若当前集合没有可改的顶层标量字段，用例打印原因后提前返回，不谎报通过。未保存切换的断言不再允许跳过：探针文档保证窗口内至少有两只可点的编辑按钮，缺任何一只都会让用例失败。

## 5. 无凭据时

- `cargo test -p fluxdb-connectors --test mongo_live` → 5 个用例全部跳过（0.00s）。
- `cargo test -p fluxdb-desktop --bin fluxdb-desktop` → Mongo E2E 跳过，其余 117 个用例照常运行（本机实测 116 passed + 2 ignored）。

零用例或全跳过**不算** MongoDB 功能验证通过；需要真实链路结论时必须注入凭据跑一遍。
