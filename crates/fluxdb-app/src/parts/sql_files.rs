/// 读取用户明确选择的本地 SQL 文本；由 UI 在后台调用，不执行 SQL、不修改原文件。
pub fn read_sql_file(path: &std::path::Path) -> fluxdb_core::Result<String> {
    use std::io::Read;
    let fail = |message: &str| Error::new(ErrorKind::Internal, message);
    if !path
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("sql"))
    {
        return Err(fail("请选择 .sql 文件"));
    }
    let mut file =
        std::fs::File::open(path).map_err(|_| fail("SQL 文件无法打开，请检查路径与权限"))?;
    if !file
        .metadata()
        .map_err(|_| fail("SQL 文件信息不可读"))?
        .is_file()
    {
        return Err(fail("请选择普通 SQL 文件"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| fail("SQL 文件读取失败"))?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let text = std::str::from_utf8(bytes)
        .map_err(|_| fail("SQL 文件不是 UTF-8 文本，请转换编码后打开"))?;
    if text.contains('\0') {
        return Err(fail("SQL 文件包含二进制内容，无法作为文本打开"));
    }
    Ok(text.to_string())
}
