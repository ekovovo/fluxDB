# SQL 编辑器测试

## 日常语法回归

```bash
cargo test -p fluxdb-desktop sql_editor_adapter::syntax_tests::
```

轻量语法测试正常运行；大文档缓存回归与性能测试标记为 `#[ignore]`，
普通 `cargo test` 默认跳过。

## 大文档专项测试

修改 SQL 编辑器的高亮、缓存或性能路径时，按需显式运行：

```bash
# 大文档首轮 tokenizer 与分批精化缓存回归
cargo test -p fluxdb-desktop large_initial_document_uses_fast_tokenizer_cache -- --ignored

# 大文档高亮、解析与 tokenizer 性能测试；串行运行避免互相干扰
cargo test -p fluxdb-desktop sql_editor_adapter::syntax_tests::large_sql_ -- --ignored --nocapture --test-threads=1
```

缓存回归测试覆盖超过 512 KiB 的文档及分批 Tree-sitter 精化，保留完整断言，
不以缩小文档规模替代大文档路径验证。MySQL 精化完成后仍保留 statement cache，
不建立全文树，因此完成条件使用 `refinement_statement_cache`，而非 `tree_valid`。

性能测试输出耗时，但没有固定耗时断言；比较结果时应保持平台、构建配置与机器负载一致。
这些测试不替代 SQL 编辑器 UI 修改所需的真实窗口交互验证。
