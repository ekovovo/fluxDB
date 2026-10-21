# FluxDB 维护说明

本目录是 `dm-pure 1.2.4` 的源码副本，由 FluxDB 自行维护。

- 来源：crates.io `dm-pure 1.2.4`
- 上游 commit：`58579165765aa764566c191d3a458b03c28db2f0`
- 上游仓库：`https://github.com/zhangtao103239/dm8_pure_rs`（已不可访问）
- 许可证：MIT，见同目录 `LICENSE`

本目录不在 Cargo workspace 内，避免第三方代码参与全仓 fmt/clippy/test。

## FluxDB 修改记录

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

