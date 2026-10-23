# SQL Server 测试

SQL Server 实库测试默认 `#[ignore]`，普通单测不需要 Podman。写入用例只操作本次运行
创建、带随机后缀的对象；补全 UI 测试只读使用下述预置表。共享持久化写入和容器端口复用必须串行。

## 启动隔离数据库

macOS 的 Podman machine 至少需要 2 GB 内存。脚本会先检查 Podman 可用性和内存，
不足时以退出码 `77` 明确跳过，不把未运行伪报为通过。

```sh
bash scripts/testing/sqlserver-smoke.sh
```

脚本创建随机名称的 `mcr.microsoft.com/mssql/server:2022-latest` 容器，默认端口
`1433`，并创建 `fluxdb_smoke` 及 `FLUXDB_SMOKE_*` 示例对象。它只清理本次容器；
已有容器、用户数据库和 dev 实例不会被改动。

需要保留容器做连续调试时：

```sh
bash scripts/testing/sqlserver-smoke.sh --keep
```

可用环境变量：

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `PORT` | `1433` | 宿主机端口；冲突时改为其他端口 |
| `SQLSERVER_IMAGE` | `mcr.microsoft.com/mssql/server:2022-latest` | 镜像 |
| `SQLSERVER_DATABASE` | `fluxdb_smoke` | 测试库 |
| `SQLSERVER_SA_PASSWORD` | 内置符合复杂度要求的临时密码 | SA 密码 |
| `SQLSERVER_MAX_MEMORY_MB` | `1536` | 容器内 SQL Server 内存上限 |

脚本输出：

```sh
export FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>'
export FLUXDB_MSSQL_DATABASE='fluxdb_smoke'
```

## 测试命令

连接器实库测试覆盖连接、数据库/schema 对象树、分页/排序/过滤、参数绑定、
索引/外键、DDL，以及有主键和 identity 的写入：

```sh
FLUXDB_PROFILE=dev \
FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>' \
FLUXDB_MSSQL_DATABASE='fluxdb_smoke' \
cargo test --locked -p fluxdb-connectors --test sqlserver_live \
  -- --ignored --test-threads=1 --nocapture
```

应用层表操作链路（`AppController` + 生产 connector）：

```sh
FLUXDB_PROFILE=dev \
FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>' \
FLUXDB_MSSQL_DATABASE='fluxdb_smoke' \
cargo test --locked -p fluxdb-app --test sqlserver_table_actions_live \
  -- --ignored --test-threads=1 --nocapture
```

无头 UI 测试使用真实生产 `NavicatMain` 与连接表单，不依赖可达 SQL Server：

```sh
FLUXDB_PROFILE=dev \
cargo test --locked -p fluxdb-desktop --bin fluxdb-desktop \
  sqlserver_ -- --nocapture
```

### SQL 补全的真实库无头测试

预先在 `fluxdb_smoke` 创建专用只读测试表（已有时不要覆盖）：

```sql
CREATE TABLE dbo.FLUXDB_COMPLETION_E2E (
    ID int IDENTITY PRIMARY KEY,
    CUSTOMER_NAME nvarchar(80) NOT NULL
);
```

```sh
FLUXDB_PROFILE=dev \
FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>' \
FLUXDB_MSSQL_DATABASE='fluxdb_smoke' \
cargo test --locked -p fluxdb-desktop --bin fluxdb-desktop \
  sqlserver_ -- --ignored --test-threads=1 --nocapture
```

其中三个补全生产 View 测试覆盖表候选限定名、手输 `[dbo].`，以及点击 schema 后继续弹表并正确插入。
应用层 `sqlserver_database_completion_includes_unopened_schemas` 另建两个随机 schema，
验证部分对象树/索引预热、未展开 schema、跨 schema 同名表和显式 schema 过滤，并清理本次对象。

### 复制表弹窗

```sh
FLUXDB_PROFILE=dev \
FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>' \
FLUXDB_MSSQL_DATABASE='fluxdb_smoke' \
cargo test --locked -p fluxdb-desktop --bin fluxdb-desktop \
  sqlserver_copy_ -- --ignored --test-threads=1 --nocapture
```

两个生产 View 无头事件用例只创建随机测试表：覆盖 29 列混合类型表的“仅结构”、
IDENTITY 表的“结构和数据”、预览保留完整换行且不可编辑、复制结果与源表不变。
默认测试文本系统为 Noop；这些结果不替代原生 AppKit/CoreText/Metal 的崩溃验证。

### 二进制 CSV 导入

```sh
FLUXDB_PROFILE=dev \
FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>' \
FLUXDB_MSSQL_DATABASE='fluxdb_smoke' \
cargo test --locked -p fluxdb-app --test sqlserver_table_actions_live \
  sqlserver_import_binary_hex_csv -- --ignored --nocapture
```

覆盖 `varbinary` 列的 `0x00FF10`、`NULL` 和空二进制 `0x`，并通过真实 connector 读回校验。
导入日志页的失败原因只在“失败”阶段显示一次；常规页和主窗口提示仍保留。

### 小栈工作线程回归

```sh
FLUXDB_PROFILE=dev \
FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>' \
FLUXDB_MSSQL_DATABASE='fluxdb_smoke' \
cargo test --locked -p fluxdb-app --test sqlserver_table_actions_live \
  sqlserver_copy_table_on_small_stack_worker -- --ignored --nocapture
```

用 512 KiB 工作线程执行真实复制调用链，覆盖“仅结构”“结构和数据”及源表不变。
修复前该用例会栈溢出并终止测试进程；普通 libtest 大栈和 Noop UI 测试不能替代这项验证。
TLS 握手由共享 Tokio 工作线程调度，调用线程只等待连接结果；证书校验与连接超时保持不变。

### 稳定性与正确性回归

连接器实库用例还覆盖：

- 同一查询标签跨多次执行保留临时表和会话状态，不同查询标签互相隔离。
- `query_timeout_secs` 在真实 `WAITFOR DELAY` 上中止等待，连接随后关闭。
- 用户取消在真实长语句执行期间生效，不再只检查语句边界。
- `geometry` 等 CLR UDT 以原始二进制承载，不再触发驱动 `todo!()` 进程 abort。
- 单次查询只保留前 16 MiB 行数据，超限结果标记截断，避免内存无界增长。
- 表复制成功时保留并重命名外键；后置约束失败时整个复制事务回滚，不留下半成品表。
- SQL Server 对象树能列出存储过程和函数，并能读取 `OBJECT_DEFINITION`；生产 View 无头测试验证左键点击后打开只读 DDL 弹窗。

## 边界与跳过条件

目标数据库选择的只读无头用例：

```sh
FLUXDB_PROFILE=dev \
FLUXDB_MSSQL_SMOKE='127.0.0.1:1433:sa:<password>' \
FLUXDB_MSSQL_DATABASE='fluxdb_smoke' \
cargo test --locked -p fluxdb-desktop --bin fluxdb-desktop \
  sqlserver_security_selects_database_without_connection_default -- --ignored --nocapture
```

该用例从未配置默认库的连接进入生产页面，通过刷新加载库列表，再使用生产 Select 搜索、
键盘确认业务库和 `master`，检查安全快照的数据库归属与连接配置不变；不执行权限写入。

- 首版只支持 SQL Server 认证和显式 TCP 端口；命名实例不在本套件验证范围。
- 测试容器不启用证书信任配置以外的特殊服务器设置；`TrustServerCertificate`
  是默认测试连接选项。
- 端口被占用、Podman 未安装/未启动、Podman 内存不足时，脚本明确跳过。
- 真实用例若缺少 `FLUXDB_MSSQL_SMOKE` 会失败，不会静默返回成功；`#[ignore]`
  只负责把实库用例排除在默认测试之外。
- 多个实库测试共享一个容器，必须使用 `--test-threads=1`。每个用例使用随机对象名
  并在结束或 panic 展开时尽力清理，不删除容器预置的 `FLUXDB_SMOKE_*` 对象。
