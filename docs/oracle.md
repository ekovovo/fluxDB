我建议把 Oracle 接入看成一次**“数据库语义适配”**，而不是“增加一种连接类型”。

看了当前项目的接入方式，除了 `Connector`，还存在独立的筛选方言、SQL 补全、表操作、导入/备份 Provider、AI SQL 安全策略。**能连接、能查出数据，只能证明最基础的一段接通，不能代表 Oracle 已完整支持。**

下面这些可以直接作为 AI coding 的需求和验收清单。

## 1. 先确定版本和驱动边界

这是实现前最该说清楚的事情，否则后面容易混入不同版本的语法。

- **明确首期支持版本**：例如先支持 19c，再单独决定是否支持 11g、12c、21c、23ai。分页、Identity、SQL Boolean、JSON 类型、标识符长度都存在版本或兼容参数差异。
- **明确驱动方案**：当前项目使用的 `sqlx` 不提供 Oracle 驱动，不能照现有 MySQL 路线直接加 feature。若选择基于 OCI 的 Rust 驱动，需要核实 Instant Client、动态库加载、授权与分发要求。
- **明确平台和架构**：Windows/Linux/macOS，以及 x86_64/ARM64 分别验证；“Rust 编译通过”不等于用户机器上客户端库能正常加载。
- **先验证驱动的关键能力**：精确 NUMBER、LOB、参数绑定、事务、取消、超时、连接失效恢复，再开始铺 UI。

**不要让 AI 在还没确认驱动限制时，先把所有功能菜单点亮。**

## 2. 连接：Service Name、SID、Schema 不是同一个东西

Oracle 连接表单不能只有“地址、端口、数据库名”。

| 关注点             | 应明确的行为                                             |
| ------------------ | -------------------------------------------------------- |
| Service Name / SID | 分开建模，不能把两种连接串混用                           |
| CDB / PDB          | 明确连接的服务对应哪个容器，不假设一个实例就是一个业务库 |
| 用户 / Schema      | 登录用户、当前 Schema、对象 Owner 分开记录               |
| TNS / Easy Connect | 哪些格式支持，哪些需要本机配置，要明确                   |
| TCPS / Wallet      | 证书校验、Wallet 路径及凭据边界明确，不默认跳过校验      |
| 特权身份           | 普通连接不默认 SYSDBA；若支持，必须显式选择和警示        |

还有两个容易漏的地方：

- `ALTER SESSION SET CURRENT_SCHEMA` **只改变名称解析，不会增加权限**，不能当成“切换登录用户”。
- SSH 隧道遇到 SCAN、多地址连接描述符、服务端重定向时，不能简单替换一个 host/port 就宣称支持 RAC。

### 已实现：请求级 schema 落到执行会话

`QueryRequest.schema` 原先只影响补全，执行端完全忽略，于是右键 schema 新建的查询会跑在
连接档案的默认 owner（未配则落到登录用户自己的 schema）上，且不报错。现在：

- 执行入口（`oracle_execute_query_with_progress` / `oracle_execute_parameterized`）通过
  `oracle_connect_with_schema` 传递 `request.schema`；
- 优先级：请求 schema → 连接档案默认 schema → 都不发 `ALTER SESSION`（保持登录用户默认）；
- 元数据、数据页等走 `ObjectPath.schema` 的路径不变，二者语义一致；
- 每次执行都是独立登录，会话随连接销毁，不存在跨请求残留。

注意 `ALTER SESSION SET CURRENT_SCHEMA` 不校验 owner 是否存在，写错名字不会在切换时报错，
而是在后续语句解析时以 `ORA-00942` 暴露。SQL Server 执行器目前只认 `request.database`，
`request.schema` 同样未生效，属于同一类缺口，尚未修改。

连接错误要区分网络、监听器、服务名、账号锁定、密码过期、客户端库缺失等类别；保留可诊断的 Oracle 错误码，但日志和 AI 上下文不得带密码、Wallet 密码或完整敏感连接串。

## 3. 对象树和元数据：不能套用 MySQL 的“数据库→表”

建议先定义清楚项目现有 `database / schema / name` 在 Oracle 中分别表示什么。

- 对象 SQL 通常使用 **`"OWNER"."TABLE"`**，不要生成 `PDB.SCHEMA.TABLE` 这样的普通三段名。
- 元数据优先按权限使用 `USER_*`、`ALL_*`；不要把 `DBA_*` 或访问 `V$*` 作为普通浏览的前提。
- 区分表、视图、物化视图、同义词、序列、过程、函数、包、触发器；暂不支持的对象应明确展示边界。
- **同义词不是表本身**：可能指向其他 Schema，甚至数据库链接；浏览、补全、AI 授权都要考虑真实目标。
- 看不到元数据不一定表示对象不存在，也可能没有权限。不能因此生成“新建同名对象”的错误建议。

尤其注意标识符：

> 未加引号的名称通常折叠为大写；加双引号的名称保留大小写并按精确名称解析。

不能为了“统一”把所有对象名转小写，也不能把用户输入的 `employees` 直接包装为 `"employees"`，误以为它一定对应普通创建的 `EMPLOYEES`。

## 4. 数据展示与编辑：最危险的是“看起来正常，实际失真”

建议把以下语义作为独立验收项：

| 类型 / 行为              | AI coding 应注意                                             |
| ------------------------ | ------------------------------------------------------------ |
| `NUMBER`                 | 不能统一转 `f64`；覆盖高精度、无显式精度/标度、负 scale 等情况 |
| `DATE`                   | Oracle DATE 包含时分秒，不是只有年月日                       |
| `TIMESTAMP`              | 保留小数秒精度；带时区与本地时区类型不能混为普通时间         |
| `VARCHAR2` / `NVARCHAR2` | 注意字符集，以及 BYTE/CHAR 长度语义                          |
| 空字符串                 | 字符串 `''` 在 Oracle 中通常按 NULL 处理；UI、筛选和导入不能假装可区分 |
| `CHAR`                   | 注意尾部填充，不能随意 trim 后再用它定位或回写               |
| CLOB / BLOB              | 有界预览、按需加载、明确截断；不能把预览内容当完整值保存     |
| RAW / XMLTYPE / 特殊类型 | 无损展示或明确只读，不静默降成可编辑普通文本                 |

补充几个关键约束：

- **空 CLOB 不等于 NULL**，不能把字符串空值规则推广到所有 LOB。
- **截断预览不能伪装成完整值**：数据页里的字符 LOB 只读 4000 字符，超过时返回
  `CellValue::TruncatedText { preview, total_chars }` 而不是拼说明文本。消费方必须按此区分：
  - 展示：格子里显示预览并标注「仅预览」，详情面板打开时后台调 `load_cell_text`
    读完整值（上限 8 MiB，超限报错），到达后替换显示并可编辑。
  - 写入/导入/逻辑备份：一律拒绝（`validate_data_changes`、导入、各备份字面量渲染），
    因为片段写回等于覆盖完整数据且不可逆。
  - 导出、复制、历史回滚 SQL：拒绝或输出 `NULL /* 截断… */`，绝不落半截值。
  - 读取判截断必须多读一次到 EOF：恰好读满 4000 字符的值是完整的，不能算截断；
    读取出错按截断处理（宁可要求重读，也不能把半截当完整）。
  - **查询必须显式 `fetch_lobs()`**：驱动默认把 CLOB/NCLOB/BLOB 改写成
    `LONG`/`LONG RAW`，于是 LOB 判断分支根本不触发——整块内容被物化进结果行
    （没有任何上界），空 CLOB 还会被压成 NULL 与真 NULL 混同。`oracle_query` 与
    编辑器执行入口都已开启；新增查询入口时同样要开，否则 LOB 又退化成无界读取。
- 时间、数字绑定和导出不能依赖本机或会话的 `NLS_DATE_FORMAT`、数字小数点等隐式设置。
  连接建立时统一发送 `ALTER SESSION SET NLS_DATE_FORMAT / NLS_TIMESTAMP_FORMAT /
  NLS_TIMESTAMP_TZ_FORMAT`（`YYYY-MM-DD HH24:MI:SS[.FF9][ TZH:TZM]`），与展示、导出文本一致；
  否则把导出文本绑回 `DATE`/`TIMESTAMP` 列会按服务端默认 `DD-MON-RR` 解析并报 `ORA-01861`。
  该设置只作用于本连接，不写回数据库或其它会话。
- 编辑行优先使用可靠主键；不能把“所有显示列相等”当成唯一身份。
- `ROWID` 若使用，只能在支持的对象和生命周期内作为受控定位手段，不能作为持久稳定主键。
- 没有可靠行身份、不可更新视图、计算列、Identity 等场景，要正确限制编辑。
- UPDATE/DELETE 必须校验真实影响行数，异常多行不能直接提交。

## 5. 筛选、排序和分页：展示、预览、执行必须一致

这部分特别容易出现“预览 SQL 是一套，真正执行又是一套”。

- **分页按支持版本实现**：12c+ 可考虑 `OFFSET … FETCH`；若支持 11g，需要正确的 `ROWNUM` 包裹方案，不能照搬 `LIMIT`。
- **排序保证确定性**：排序值重复时，可在适用场景追加主键作为稳定次序；否则翻页会重复或漏行。
- 明确 NULL 的排序行为，必要时使用 `NULLS FIRST/LAST`，不要继承 MySQL 的默认假设。
- 不提供假的 `ILIKE`；大小写、重音、语言排序应按 Oracle 能力和配置定义。
- `LIKE` 的 `%`、`_`、转义符要正确处理；`IN` 列表上限按版本处理，不能无限生成。
- LOB 等类型不支持普通等值比较或排序时，要限制操作符，或提供有明确语义的专门实现。

**所有筛选值必须真实 bind；标识符则走统一引用和校验。** 不要用字符串替换来模拟参数绑定。

当前项目已有统一 `FilterExpression`，Oracle 应接入这一条链，不要另写一套 UI 拼 WHERE。

### 已实现：通用查询的行数上限

编辑器查询与 AI 取数在 app 层统一给 SELECT 封顶（`sql_text_for_execution`），原先只区分
「SQL Server 用 TOP、其余拼 LIMIT」，导致 Oracle 收到 `... LIMIT 100` 报 `ORA-00933`。
现已把「后缀形式的上限子句」下沉到 `SqlDialect::render_row_limit_suffix`：

| 方言 | 追加形式 | 备注 |
| --- | --- | --- |
| MySQL / TiDB / SQLite / PostgreSQL / 达梦 | ` LIMIT n` | 已有 `LIMIT` 时只在超限时改写 |
| Oracle | ` FETCH FIRST n ROWS ONLY` | 12c+；不要求 ORDER BY；已有 `OFFSET`/`FETCH` 时原样放过 |
| SQL Server | 返回 `None`，改插 `TOP (n)` | `TOP` 在列位置，无法后缀追加 |

配套约束：

- 上限子句必须插在锁定子句 `FOR UPDATE` / `FOR SHARE` 之前，否则三个方言都是语法错误。
- Oracle 的 row limiting 子句与 `FOR UPDATE` 互斥（同用报 `ORA-02014`），语句带锁定子句时跳过封顶，
  交给服务端执行——为了封顶把语句改到报错是不可接受的。
- Oracle 的 `/*+ … */` 优化器 hint 在去注释时保留；`#` 不当作注释起始符（MySQL 规则会吃掉合法 SQL）。
- `page_size == 0`（设置里选「不限制」）不追加任何上限子句。
- **`WITH … SELECT` 同样封顶**：过去判据只看语句是否以 `SELECT` 开头，CTE 完全绕过默认上限，
  一条 `WITH x AS (…) SELECT * FROM 大表` 就能把整表拉进内存。现在改由 `sql_selects_rows`
  判定「顶层第一个查询/写入关键字是 `select`」。`WITH … INSERT/UPDATE/DELETE/MERGE` 是写入，
  加上限会静默截断影响行数，所以仍不加；CTE 定义体在括号里，不会被误认成主语句。

新增方言只需实现该 trait 方法，不再改 app 层的封顶逻辑。

## 6. 表右键：设计、复制、清空、备份要分别定义契约

### 设计表

Oracle 设计表必须适配现有字段类型与属性能力映射机制，覆盖元数据回显、参数语义、输入校验、类型切换及 DDL 往返。不得仅添加类型下拉选项；不得丢失 BYTE/CHAR、负标度、时区或未指定参数的语义。验收重点是：未修改的字段不产生额外 ALTER，也不发生定义损失。

Oracle 需要独立处理：

- Identity 与“序列＋触发器”两种自增模式，不能显示成 MySQL `AUTO_INCREMENT`。
- 虚拟列、默认表达式、字段注释，以及字符长度语义。
- 约束的启用、验证、延迟检查状态。
- 唯一约束和背后支撑索引之间的关系，不能重复创建或误删。
- 不支持编辑的高级属性要保留、提示或拒绝修改，不能在保存时静默丢失。

**核心红线：Oracle 的许多 DDL 会隐式提交。**

设计表多条变更不能承诺“任意失败全部回滚”。应提供 SQL 预览、顺序、不可回滚提示，以及失败时哪些步骤已经完成。也不要和用户尚未提交的 DML 共用一个会话随意执行 DDL。

### 复制表

要区分：

- 只复制数据；
- 列结构＋数据；
- 包含约束、索引、触发器、注释等的完整结构复制。

`CREATE TABLE … AS SELECT …` **不是完整克隆**，不会自动完整复制索引、外键、触发器、授权等。

完整复制还要处理约束/索引名称冲突、Identity/序列关系、触发器是否启用、外键是否仍指向源表。不能只对 DDL 做全局字符串替换。

当前实现：复制表先经 `table_ddl` 读取源表 DDL，再按语句结构重写表名、约束名、索引名和注释目标，`结构和数据` 额外生成显式列表的 `INSERT … SELECT`（跳过 `GENERATED ALWAYS AS IDENTITY` 列）。触发器与授权不在复制范围内。UI 与 controller 共用 `copy_table_requires_source_ddl` 判定是否需要预读 DDL，避免两处白名单漂移。

### 清空和删除

- `DELETE` 与 `TRUNCATE` 的事务、触发器、权限、外键行为不同，必须分别说明。
- “清空后导入失败”不能宣称数据会自动恢复。
- 删除对象的级联影响应预览，不自动附加危险选项。
- 不要用 DROP 后重建来绕过 ALTER 限制，除非用户明确批准数据和依赖风险。

### 备份

把三个概念分开：

1. 数据导出；
2. DDL＋数据的逻辑导出；
3. Data Pump / RMAN 等原生能力。

尤其是 **Data Pump 的 DIRECTORY 指向数据库服务器端目录，不是桌面客户端路径**。不能把用户选的本地目录直接传成服务端导出位置。

首期完全可以不做原生备份，但必须明确叫“数据导出”或“逻辑导出”，不能提供一个看似完整可恢复的“备份”按钮。

## 7. 导入导出：需要无损、事务边界和一致性

- NUMBER、日期、时区、NULL、换行、二进制和 LOB 要有明确且可往返的格式。
- CSV 空字段如何映射 NULL，应明确；不能承诺在 Oracle 字符串列中保留 NULL 与空字符串的区别。
- 使用绑定和批量能力；导入预览、类型转换与实际执行使用相同规则。
- 明确整批事务还是分批提交；取消或失败后，报告已提交、已回滚和未处理的范围。
- 触发器、默认值、Identity、约束可能改变导入结果，不能默认全部禁用。
- Oracle 常见版本不能直接套用其他库的多行 `VALUES`、UPSERT 语法；按版本选择批量绑定等实现。

导出一致性也要特别注意：

> Oracle 默认读已提交下，多次独立分页查询不天然共享同一个快照。

大表导出应评估单游标流式读取、只读事务或有权限约束的 SCN 方案；同时处理长查询的 undo 保留限制和 `ORA-01555`，不能把多页各自查询叫作“一致快照导出”。

## 8. 用户与权限：不是 PostgreSQL Role 页面换个标题

Oracle 至少要分清：

- 用户与 Schema 的关系；
- 系统权限、对象权限、角色；
- 直接授权、角色继承、PUBLIC；
- `WITH ADMIN OPTION` 与 `WITH GRANT OPTION`；
- 表空间、Quota、Profile、锁定和密码过期状态；
- CDB/PDB 中的公共用户与本地用户。

几个具体陷阱：

- 创建用户不等于具备登录、建表和分配空间的全部条件。
- `CURRENT_SCHEMA` 不提供访问权限。
- 定义者权限 PL/SQL 中的权限规则，与交互会话通过角色获得的权限不完全相同。
- **继承来的权限不能显示成可直接撤销的授权。**
- 用户、授权等管理 DDL 同样不能复用 PostgreSQL“单事务全回滚”的承诺。
- 密码变更 SQL 不进入历史、日志、AI 工具回执。
- 交互与 MySQL / PostgreSQL / SQL Server 一致：草稿不需要单独“加入变更”，点“保存”先弹出
  统一 SQL 确认框（`admin.pending_sql`），展示本次要执行的语句，点“执行”才提交；“SQL 预览”
  页显示同一份语句，保证“看到的就是跑的”。预览按草稿原样显示口令（与 MySQL / PostgreSQL
  一致），执行回执另渲染脱敏副本，仅用于回显。
- 新建账号支持一次保存带多条授权：授权编辑器一次只放一条，点“加入待执行”后继续添加，
  角色 / 系统权限 / 对象权限可混排，保存时全部并入同一份计划。这解决了“必须先建号再回来
  授权”的两次来回——代价是计划里多条语句依旧逐条提交，无法整体回滚。
- 右栏五个页签：常规 / 角色 / 系统权限 / 对象权限 / SQL 预览。三份授权清单本来就是
  互相独立的目录（`USER_ROLE_PRIVS` / `USER_SYS_PRIVS` / `ALL_TAB_PRIVS`），
  所以每个页签顶部各有一个**类型锁死**的授权编辑器，不再让用户先选类型再填名字；
  待执行列表只显示本页类型的条目，删除仍按全局下标，避免跨页删错。
- 授权名的候选来自 `USER_ROLE_PRIVS` / `USER_SYS_PRIVS` 里**带 ADMIN_OPTION 且非继承**
  的行：只有这些能转授。候选只是输入提示，普通账号看不到完整角色目录，
  列表为空时仍可手填任意名称，不报错。
- 「用户与权限」的实现集中在 `apps/fluxdb-desktop/src/main_parts/user_admin/`：
  `shared.rs` 是公共壳（工具栏度量、页签条、SQL 确认框、按钮）与 `UserAdminPanel` trait，
  `mysql/`、`postgres/`、`sqlserver/`、`oracle/` 各放一个库的面板实现。
  库类型到面板只有 `user_admin_panel_for` 一个分支点，新增库不再改动其它库的代码。
  `UserAdminDetailTab` 也因此拆成四个按库独立的枚举，一库加页签不会打断其它库的穷尽匹配。
- 达梦 DM8 复用本面板，方言差异只在 `UserAdminSqlDialect::Dameng`：登录权限改为
  `GRANT CREATE SESSION`（达梦无 `CONNECT` 角色）、`CREATE USER` 不带 `PROFILE`
  （建号后 `ALTER USER … PROFILE`）、无 `QUOTA … ON …`。
- 两条执行侧约束（都踩过真实库才暴露）：
  - 渲染结果带结尾分号是给预览看的，OCI 不接受分号（`ORA-00922`）。执行前必须去掉，
    与达梦 `dm_split_statements` 的做法保持一致。
  - 建号语句会把账号名折叠为大写，因此**同一次保存里的授权必须用同一个大写名字**做被授权人；
    否则 `GRANT … TO "new_user"` 打不到刚建的 `"NEW_USER"`。角色 / 权限名走目录名，保留原样。

建议把用户权限管理作为独立阶段，不要为了功能齐全而先套用其他数据库的数据模型。

## 9. SQL 编辑器和补全：PL/SQL 是一个重点

Oracle 不只是普通 SQL 关键字有差异，**语句边界也不同**。

- `BEGIN … END;`、过程、函数、包、触发器内部含分号，不能按分号简单拆句。
- `/` 通常是 SQL*Plus 风格客户端执行分隔符，不是可以原样交给驱动的普通 SQL。
- 区分发送给驱动的 SQL、PL/SQL，以及 `SET`、`SPOOL`、`PROMPT`、`@file` 等客户端命令；不支持就明确拒绝。
- 正确处理 `q'[...]'` 等替代引号、注释和字符串中的分号。
- SQL 和 PL/SQL 对结尾分号的处理不能一刀切。

### 已实现：两层的脚本切分

`crates/fluxdb-connectors/src/script_syntax/` 把切分拆成两层，避免再出现「一条规则套所有方言」：

| 层 | 负责 | 实现 |
| --- | --- | --- |
| 词法 | 引号、注释、转义、`q'…'`、`nq'…'`、`$$…$$`、`[…]` | `sqlparser::dialect::Dialect`（现成，按连接类型选方言） |
| 边界 | 块深度、语句头形态、`/` 与 `GO` 批分隔符 | `ScriptSyntax` trait，Oracle / 达梦共用 `ORACLE_SYNTAX`，SQL Server 另有专属实现 |

推演要点，都是踩过的坑：

- **按语句类别决定结束条件，而不是全局一条规则。** 普通 SQL 见到 `;` 即结束；
  `DECLARE` / `BEGIN` / 过程 / 函数 / 包的声明段（首个 `BEGIN` 之前）不切，
  否则 `DECLARE x NUMBER;` 会被当成一条完整语句发出去。
- **包体自带一层块。** `CREATE PACKAGE BODY` 的初始化段 `BEGIN` 与包本身共用一个 `END`，
  深度按设计回不到 0，所以这类语句靠独占一行的 `/` 收尾——与 Flyway 的 `PLSQL_DELIMITER` 同口径。
- **`END IF` / `END LOOP` / `END CASE` 只关闭已开启的同一层**，不能重复计数；
  否则 `SELECT CASE … END` 之后真正的 `LOOP` 会被吞掉。
- **同名不同义要按相邻词排除**：`GOTO end_loop` 的标签、`INTO case` 的标识符、`BEGIN TRAN` 的事务
  （T-SQL，无配对 `END`）都不是块起点。
- **反斜杠在 Oracle 里不是转义符**，`'C:\'` 是完整字符串；`#` 是合法标识符字符，不是行注释。
  这两条过去被共享切分器按 MySQL 规则处理，会把相邻语句黏成一条。
- **同一个词在不同语句里含义不同，判定要带语句形态。** `BEGIN` 在触发器体里开启一层块，
  在 PostgreSQL/SQLite/MySQL 顶层单独出现时是开启事务、没有配对的 `END`。
  `opens_block` 因此收到当前 `StatementForm`，只在语句头已判定为块时才把 `BEGIN` 当块起点；
  否则 `BEGIN; INSERT …; COMMIT;` 会被整段吞成一条。
- **达梦与 Oracle 共用同一套块规则。** 达梦的 PL/SQL 同源：`BEGIN … END;` 匿名块、
  `DECLARE` 声明段、`END IF`/`END LOOP` 配对、独占一行的 `/` 都要照 Oracle 处理；
  达梦也没有「`BEGIN;` 开启事务」这种写法（事务由首个 DML 隐式开启）。
  这条曾被漏掉——达梦落在 `DefaultSyntax`（只认 `CREATE TRIGGER`），
  匿名块被按 `;` 切成多条，编辑器里按 Run 会把块拆开执行。
  由 `dameng_blocks_follow_oracle_boundaries`（连接器）与
  `dameng_block_stays_one_statement_in_the_editor`（桌面 code lens）两条用例锁住。
- **词法器失败时回退历史扫描器**，例如 Oracle 条件编译 `$IF $$x` 会被当成美元引号。
  这类输入本来就切不干净，回退保证不比从前更差。

### 已实现：编辑器与执行侧共用同一套边界

切分规则只有一份，两条消费路径都走它，否则「编辑器里按 Run」和「执行 SQL 文件」看到的是不同语句。

| 路径 | 入口 |
| --- | --- |
| 执行（脚本文件、AI/MCP） | `script_syntax::split` → `split_sql_statements` |
| 编辑器（Run / 折叠 / code lens / 诊断 / 语句状态行） | `fluxdb_app::sql_statement_byte_ranges`（即 `statement_ranges`） |

编辑器侧的约定：

- `SqlAdapter` 与 `SqlLanguage` 同时持有 `SqlDialect`（高亮 / 补全 / 注释标记）与
  `DatabaseKind`（切分）。二者仍需分开传：切分要的是连接类型，`SqlDialect` 只是它的
  一个投影；`SqlDialect::from_database_kind` 现在不再有损（Oracle / 达梦各有变体），
  但把两件事混成一个字段，将来再出现「同方言、不同脚本规则」的库时会重新踩坑。
- `split_statements` / `statement_around` / `build_statement_runs` / `split_statement_ranges_snapshot`
  一律显式收 `DatabaseKind`，调用方从连接配置取（`query_editor_database_kind` / `connection_database_kind`）。
  不给默认值：漏传会编译失败，而不是静默退回 MySQL 规则。
- 快照路径把 Rope 拼成 `&str` 再交给词法器。切分点（折叠、诊断、执行定位）都在编辑后的单次扫描里，
  复制一次远小于「切错语句再执行」的代价；原先逐字节的零拷贝扫描器保留为词法器失败时的兜底。

块语句切出的文本**保留结尾分号**（`BEGIN … END;` 少了它就是语法错误），普通 SQL 仍按历史行为去掉分号。

### 已实现：补全与高亮按连接类型分方言

补全引擎的 `CompletionDialect` trait（`parser_dialect` / `keywords` / `functions` /
`identifier_quote` / `qualify_bare_table` / `supports_routines`）本来就是每库一份实现对，
Oracle 有独立的 `ORACLE`（`ROWNUM`、`CONNECT BY`、`NVL`、`LISTAGG`，无 `LIMIT`、无反引号）。
缺口在桌面编辑器这一层：

- `SqlDialect` 原先只有 4 个变体，`from_database_kind` 把 Oracle / 达梦折成 `Postgres`，
  于是语义诊断（`fluxdb_completion::diagnose`）用的容错解析器、本地兜底候选与词法高亮
  全都拿到了 PostgreSQL 的词表。现在 `SqlDialect` 增加 `Oracle` / `Dameng` 两个变体，
  `completion_dialect` 按方言一一分派，`SqlDialect::database_kind` 与 `from_database_kind` 互逆。
- Oracle / 达梦的关键字表不再手抄，直接由 `fluxdb_completion::ORACLE` / `DAMENG` 的
  `keywords()` + `functions()` 惰性生成（`KeywordDef::matches` 做大小写不敏感比对，
  兼容引擎的大写词条）。同一张表喂给词法高亮、本地补全候选项与 `is_keyword`，
  「高亮认的词」和「补全给的词」不会再分叉。
- `ORACLE_KEYWORDS` 补齐了 PL/SQL 词：`DECLARE` / `BEGIN` / `EXCEPTION` / `PROCEDURE` /
  `FUNCTION` / `PACKAGE` / `LOOP` / `IF` / `ELSIF` / `RAISE` / `NEXTVAL` / `CURRVAL` 等。

补全侧仍未完成的部分：
- 包名、包成员、重载过程、参数方向、同义词，需要专门的补全规则。
- `begin` 仍是 Oracle 的关键字之一，`BEGIN` 与 `END` 同名场景（事务 vs 块）在高亮上无法区分——
  高亮只需要颜色，切分才需要语义，后者已由 `script_syntax` 处理。

### 已实现：端到端覆盖

补全与切分都在**真实生产视图**（`NavicatMain` + 真实 `Editor`）里用无头事件验证，
断言用户能观察到的结果而不是 provider 内部字段；无需连库，元数据走内存注入。

| 用例 | 位置 | 锁住的契约 |
| --- | --- | --- |
| `oracle_connection_completes_oracle_functions_not_postgres_ones` | `i18n_tests/oracle_completion.rs` | Oracle 连接补出 `NVL`/`NVL2`，补不出 `NOW` |
| `oracle_and_postgres_tabs_diverge_on_the_same_prefix` | 同上 | 同一视图两个标签页对 `SELECT TO_` 给出不同词表 |
| `oracle_table_completion_inserts_quoted_qualified_name` | 同上 | 点候选后文本为 `"EMPLOYEES"`（按方言引用） |
| `oracle_block_stays_one_statement_in_the_editor` | 同上 | `BEGIN … END;` + `SELECT` = 2 个 code lens |
| `dameng_block_stays_one_statement_in_the_editor` | 同上 | 达梦匿名块同样不被 `;` 切碎 |
| `candidates_follow_the_requested_dialect` 等 3 条 | `crates/fluxdb-completion/src/engine.rs` | 补全引擎词表、`qualify_bare_table`、`quote_ident` 按方言 |
| `oracle_keywords_carry_plsql_and_drop_limit` 等 3 条 | `sql_editor_adapter/dialect.rs` | 编辑器方言映射与词表生成 |
| `dameng_blocks_follow_oracle_boundaries` | `crates/fluxdb-connectors/src/script_syntax/mod.rs` | 达梦块边界与 Oracle 同源 |

**补全可以容错，执行安全判定不能跟着容错。** 解析器不认识 Oracle 语法时，可以降低补全质量，但不能因此把 SQL 判成安全。

## 10. AI Agent Tool / MCP：这是最需要单独设计的部分

不要把 Oracle 加进“支持 SQL 的数据库列表”就算工具适配完成。

### 只读判定

这些都不能仅因为以 SELECT 开头就自动放行：

- `SELECT … FOR UPDATE`：会获取锁。
- `sequence.NEXTVAL`：推进序列，不是纯只读。
- 调用用户函数：可能有副作用，尤其涉及自治事务。
- 文件、网络、调度、动态 SQL 等包能力。
- 经同义词、视图、函数或数据库链接间接访问其他对象。

应在现有 AST 策略基础上增加 Oracle 规则；未知语法、无法证明的副作用应进入审批或拒绝。**函数黑名单不是完整安全边界**，还需要最小权限账号、服务端约束和对象授权范围。

### 对象范围

AI 授权至少要绑定到连接、服务/容器、Schema 和对象。

同义词、数据库链接、视图和定义者权限程序单元可能扩大实际可访问范围，不能只检查 SQL 里出现的表名。无法证明范围时，不自动放行。

### 执行计划

Oracle 不能直接复用现有的 `EXPLAIN <SQL>`。

`EXPLAIN PLAN FOR` 会写计划表；读取已有游标计划则有另外的权限和信息边界。应明确实现哪一种，不能把“执行计划工具”直接当成完全无副作用的普通只读查询。

### 已实现：只读入口在执行前拒绝

`QueryExecutionOptions.read_only`（AI / MCP 路径）原先只在执行后看结果：语句真跑过，发现是
DDL 才回一句「已隐式提交且无法回滚」。这不能算只读防护，因为 Oracle 的 DDL 会连同批内
此前未提交的 DML 一起自动提交，`NEXTVAL`、自治事务、dblink 写入也不随回滚撤销。
现在这一入口是两层：

1. **执行前拒绝**：`prepared.build()` 之后按驱动解析出的语句类型判断，只放行
   `is_query()`（`SELECT` / `WITH ... SELECT`）。DDL、DML、PL/SQL 块都没机会执行。
2. **服务端只读事务**：建连后、执行任何语句之前发 `SET TRANSACTION READ ONLY`
   （排在 `ALTER SESSION SET CURRENT_SCHEMA` 之后，因为 `ALTER SESSION` 会隐式提交）。
   分类器漏掉的 DML 和 `SELECT ... FOR UPDATE` 由服务端报 `ORA-01456`，不会先改行再回滚。

收尾 `rollback()` 失败不再只是 `tracing::warn!` 后仍返回成功：只读入口的「未写入」结论依赖
回滚真的成功，失败即返回错误。

已知边界（属于 Oracle 语义，不是实现缺口，只能靠上层 AST 策略兜住）：

- `seq.NEXTVAL` / `CURRVAL` 推进不受只读事务约束，也不随回滚撤销；
- PL/SQL 中的 `PRAGMA AUTONOMOUS_TRANSACTION` 独立提交；
- 经 dblink 的写入不受本地事务约束；
- `SYS` 登录不受 `SET TRANSACTION READ ONLY` 约束。

### 写入与取消

- Oracle 连接器真正实现事务内影响行数复核之前，不声明支持有界写入。
- DDL 不走“超限回滚即可”的普通 DML 路线。
- UI 超时不代表数据库执行已停止，需要驱动级取消和会话清理。

#### Oracle 的取消只有「安全点」，必须先说清语义

`oracledb` 26.0.0-beta.4 没有 cancel/break API（全仓 `grep -ri cancel` 零命中），客户端唯一的杠杆是
`Connection::set_call_timeout`——它是**socket 读超时**，触发后驱动会先向服务端发
`MARKER_TYPE_INTERRUPT` 再重置会话，返回 `ErrorKind::CallTimeoutExceeded`。据此实现：

| 场景 | 行为 |
| --- | --- |
| 用户点「停止」，语句正在服务端计算 | 检查点停在两次取行之间，取消**不立即生效**；要么语句返回，要么连接档案里的「查询超时」触发 INTERRUPT |
| 用户点「停止」，语句正在持续吐行 | 一个取行批次内响应，收尾为「已取消：结果待核实」，后续语句标「未执行」 |
| 查询超时触发 | 驱动发 INTERRUPT，收尾为「已超时：结果待核实（…）」，本批次剩余语句标「未执行」 |
| 页满 | `rows.len() >= capacity` 立即 `break`，不再把游标抽干；或累计字节超 16 MiB 时同样停手并标 `has_more` |

页满即止之后，摘要里的行数只是「读到多少行」，不是结果集总行数——所以这条消息用
「已读取 N 行，当前仅展示 M 行（结果已截断）」，不能沿用 PG/达梦的「返回 N 行」，
否则会把没读完的部分谎报成不存在。

宁可如实报「结果待核实」，也不谎报成功或谎报已停止：服务端可能已经改了一部分行。
`query_history` 以消息里是否含「结果待核实」判定未知状态，这两条消息必须保留该关键字。

想让「停止」在计算阶段也生效，只能在连接档案里把「查询超时」设成非 0（驱动层唯一可用的打断手段）。
- 提交期间断网可能导致结果未知，**不能自动重放写入**。已实现：提交失败按错误来源分类——
  服务端回了错误报文（事务已被终止）仍是确定失败；传输层失败（断连/超时）标成
  `ErrorKind::OutcomeUnknown`，AI 工具回执为 `Unknown` 并停止后续调用，编辑器显示「结果未知」
  且不提供重试。同一个映射覆盖达梦、SQL Server、PostgreSQL、MySQL、SQLite 的提交收尾。
- 继续沿用结果行数、字节数、LOB、执行时间和并发上限，不为 Oracle 另开无界通道。

当前页面 AI 上下文可增加 Oracle 方言、版本、定位和能力摘要，但不自动上传连接描述符、凭据或业务数据值。

## 当前项目里，建议要求 AI 逐项审计的入口

这些已有边界应该复用，而不是往 UI 里继续加数据库分支：

| 接入面               | 当前入口                                                     |
| -------------------- | ------------------------------------------------------------ |
| 连接类型与配置       | `/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/connection.rs` |
| 连接器契约及创建     | `/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/connector.rs`、`/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/parts/factory.rs` |
| 标识符、限定名、分页 | `/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-core/src/parts/sql_dialect.rs` |
| 筛选解析和编译       | `/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-connectors/src/filter_dialects/mod.rs` |
| 表复制等操作         | `/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/create_table_actions.rs` |
| SQL 补全             | `/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-completion/src/dialect.rs` |
| AI SQL 安全策略      | `/Users/shining3d/fusuwei/code/fluxDB/crates/fluxdb-app/src/parts/ai_sql_policy/mod.rs` |

这里有两个现成的契约风险值得提前提醒 AI：

- `Connector` 的默认表达式读取实现会回退到无筛选读取，Oracle 必须真正实现筛选链路，不能只为通过编译保留默认行为。
- 默认导出是独立分页读取，不保证跨页一致快照；Oracle 不能未经实现就声称支持一致导出。

另外，**可以参考达梦的接线，但不能把达梦实现复制后改名当作 Oracle 支持**：当前达梦使用的分页、解析方言和安全规则都不能直接证明适用于 Oracle。