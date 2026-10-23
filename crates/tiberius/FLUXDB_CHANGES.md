# FluxDB 维护说明

本目录是 `tiberius 0.12.3` 的源码副本，由 FluxDB 维护。

- 来源：crates.io `tiberius 0.12.3`
- 许可证：MIT / Apache-2.0，见同目录 `LICENSE-*.txt`
- 上游仓库：https://github.com/prisma/tiberius

## 为什么内置

上游 0.12.3 在列元数据解码与行值解码中都没有实现 `sql_variant`：

- `src/tds/codec/type_info.rs` 的 `SSVariant` 长度分支落到 `todo!()`；
- `src/tds/codec/token/token_col_metadata.rs` 的 `SSVariant` 直接 `todo!()`；
- `src/tds/codec/column_data/var_len.rs` 的值解码同样没有 `SSVariant`。

任何结果集只要包含 `sql_variant` 列（例如用户编辑器执行 `SELECT *`），
整个桌面进程会在 TDS 解码线程里 abort。数据页和导出预览可以用显式
列清单绕过，但用户 SQL 不能透明改写，因此必须在驱动层修复。

## 补丁内容

- `TypeInfo::decode` / `Encode`：`SSVariant` 按协议读取/写入 4 字节长度；
- `BaseMetaDataColumn::null_value`：`SSVariant` 返回 `ColumnData::Binary(None)`；
- `var_len::decode`：`SSVariant` 的载荷按原始二进制返回，不猜测基础类型。

上层 `fluxdb-connectors` 将原始字节渲染为十六进制，并标明这是
`sql_variant` 原始值；不声称已解析出内部基础类型。

## 同步上游

升级 tiberius 前先检查上游是否已实现 `SSVariant`；若已实现，优先移除
本副本并恢复 crates.io 依赖，不继续维护分叉。
