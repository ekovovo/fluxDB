## 新功能

- 新增 Oracle 数据源：支持 Service Name / SID 两种连接方式与可选 Wallet，连接后按 Schema 组织对象树，可浏览表、视图、同义词等对象。
- Oracle 支持 SQL 查询与方言补全、执行取消、表结构查看与设计、数据编辑、筛选与排序，以及本机逻辑导出（DDL + 逐页数据）。
- Oracle 接入 AI 页面上下文与受控执行：`SELECT … FOR UPDATE`、序列 `NEXTVAL`、`dbms_*` / `utl_*`、同义词与 `@dblink` 间接访问、`EXPLAIN PLAN FOR` 均按写操作处理，需要人工审批。
- 用户与权限面板扩展为 常规 / 角色 / 系统权限 / 对象权限 / SQL 预览 五页签；Oracle 与达梦各页签顶部是类型锁死的授权编辑器，可连续「加入待执行」后一次保存。
- Oracle 支持新建账号、修改口令、锁定 / 解锁、表空间与配额、Profile 与登录授权；「新建 schema」直接打开新建账号表单，不再创建无法登录的随机口令账号。
- 达梦 DM8 复用同一账号面板，按方言使用 `CREATE SESSION`、`ALTER USER … PROFILE` 等写法。

## 改进

- 用户与权限代码按数据库拆分为独立面板（MySQL / PostgreSQL / SQL Server / Oracle），公共壳与页签条收敛到共享模块，新增数据库不再改动其它库的实现。
- 命令分发按连接、标签页、用户权限拆分，降低 macOS 调试构建的线程栈占用（约 287 KiB → 90 KiB），命令行为与补全失效逻辑保持不变。
- Oracle 的 ER 外键与补全列表改为按 Owner 一次登录批量读取，替代逐表 N+1 查询，并在建连前、分片之间与查询返回后响应取消。
- Oracle 对象树行数取自 `ALL_TABLES.NUM_ROWS` 统计值，不触发全表扫描。
- 重新加载表数据时只遮罩表格区域，工具栏、筛选面板与 SQL 预览保持可见。

## 修复

- 修复 Oracle 导入时因服务端默认日期格式导致的 `ORA-01861`：会话固定 `NLS_DATE_FORMAT` 等 ISO 格式。
- 修复 Oracle 复制表始终报「未读取到源表 DDL」。
- 修复 Oracle 与 SQL Server 的 Explain 按钮会发出必然解析失败的语句，现按方言禁用工具栏按钮与快捷键。
- 修复 Oracle 账号变更失败提示缺少已执行条数的问题，便于按实际生效情况重跑剩余语句。

## 验证

- 通过 `cargo fmt --all -- --check`、`cargo check --workspace --locked` 和 `cargo clippy --workspace --all-targets --locked`。
- 通过 Oracle 方言补全、只读判定与权限约束单测，用户权限五页签工具栏与「一次保存」流程测试，以及重新加载遮罩范围的无头 E2E。
- Oracle 真库用例（导入回归、批量元数据读取）位于 `crates/fluxdb-connectors/tests/oracle_live.rs`，需要本地 Oracle 实例，不在自动门禁内。
