# FluxDB 维护说明

本目录是 `dm-pure 1.2.4` 的源码副本，由 FluxDB 自行维护。

- 来源：crates.io `dm-pure 1.2.4`
- 上游 commit：`58579165765aa764566c191d3a458b03c28db2f0`
- 上游仓库：`https://github.com/zhangtao103239/dm8_pure_rs`（已不可访问）
- 许可证：MIT，见同目录 `LICENSE`

本目录不在 Cargo workspace 内，避免第三方代码参与全仓 fmt/clippy/test。

## FluxDB 修改记录

### 1.2.4-fluxdb.2

- 修复批量参数化 INSERT 被误判为查询结果集的问题。`cmd=187` 帧既可能是结果集，
  也可能是 DML 影响行数确认；旧实现只看帧长（>30 字节）判定为结果集，
  而参数化 INSERT 4 行时确认帧正好 36 字节，于是既不返回行数也不报错，
  还会因残留确认帧让下一条语句报“无效的语句句柄”。现在先看列数，列数为 0
  一律按 DML/CALL 处理。
- 新增 `tests/dm_batch_probe.rs` 覆盖 1/2/3/4/5/8/13/50 行的批量插入。

### 1.2.4-fluxdb.1

- 结果集文本改为严格解码（`decode_text_strict`）。此前 `encoding_rs::decode` 遇到非法
  字节会静默写入替换字符，损坏数据会被当成正常文本展示；现在 VARCHAR/CLOB 解码失败
  会返回协议错误，避免静默产生乱码。
- 出错信息（`decode_lossy`）仍保留宽松解码，避免服务端报错文本损坏时吞掉错误本身。
- 新增 `rustfmt.toml` 关闭格式化，保持与上游 1.2.4 的 diff 只含功能改动。
- 新增单测覆盖 UTF-8 合法文本、UTF-8 会话遇 GBK 字节报错、GB18030 正常解码。

说明：本次未改动字符集协商与握手逻辑。实测 DM8 UTF-8/GB18030 两种会话的写入、内联
SQL 写入、参数绑定与读取往返均正确；历史 demo 数据中的乱码属于“UTF-8 字节被按
GB18030 解码后再以 UTF-8 存储”的脏数据，已通过 `tests/dm_repair_mojibake.rs` 按严格
可逆规则还原并复核。

