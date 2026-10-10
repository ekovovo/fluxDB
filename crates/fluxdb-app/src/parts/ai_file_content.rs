// 附件内容解析：文本按字节分段、CSV 按记录流式分段、二进制只回类型与大小不硬解。
// 三条路径共用同一份安全句柄，不接受模型提供的任意路径。

/// 判定附件是否为按字节读取的文本，还是需要 CSV 记录分段或二进制反馈。
fn ai_file_delimited_format(path:&std::path::Path)->Option<u8> {
    match path.extension().and_then(|value|value.to_str()).map(str::to_lowercase).as_deref() {
        Some("csv")=>Some(b','),
        Some("tsv")=>Some(b'\t'),
        _=>None,
    }
}

/// 二进制判定：允许 UTF-8 BOM，其余位置出现 NUL 或无效多字节序列即视为二进制。
/// 只看已读窗口即可给出类型反馈，完整文件类型由 file.read 每段重复校验。
fn ai_bytes_are_binary(bytes:&[u8])->bool {
    let body=bytes.strip_prefix(&[0xEF,0xBB,0xBF]).unwrap_or(bytes);
    if body.contains(&0) {return true;}
    std::str::from_utf8(body).is_err()
}

/// 二进制反馈：给出判定依据与可读预览，不把原始字节塞进模型上下文。
/// 预览仅保留可打印 ASCII，避免控制序列与半个多字节字符进入提示词。
fn ai_binary_preview(bytes:&[u8])->String {
    let printable=bytes.iter().filter(|byte|byte.is_ascii_graphic()||**byte==b' ').count();
    let total=bytes.len().max(1);
    let ratio=printable*100/total;
    let head=bytes.iter().take(64).map(|byte|if byte.is_ascii_graphic(){*byte as char}else{'.'}).collect::<String>();
    format!("可打印字符约 {ratio}%；前 64 字节：{head}")
}

struct AiFileSegment {
    value:serde_json::Value,
    next_offset:u64,
}

/// CSV 分段：从 offset 起流式解析最多 max_records 条记录，返回记录数与下一个可续读字节位置。
/// 引号转义与嵌入换行交给 csv crate；解析错误带上行号，便于模型定位脏数据而不是静默截断。
fn ai_read_csv_segment(
    path:&std::path::Path,
    mut file:std::fs::File,
    offset:u64,
    max_records:usize,
    total_len:u64,
)->fluxdb_core::Result<AiFileSegment> {
    use std::io::{Seek,SeekFrom};
    let delimiter=ai_file_delimited_format(path).unwrap_or(b',');
    file.seek(SeekFrom::Start(offset)).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件偏移无效"))?;
    let mut reader=csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(file);
    let mut rows:Vec<Vec<String>>=Vec::new();
    for (index,record) in reader.byte_records().take(max_records).enumerate() {
        let record=record.map_err(|error|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,format!("CSV 解析失败（第 {} 条记录附近）：{error}；可缩小范围或改读文本",error.position().map(|position|position.record()).unwrap_or(index as u64))))?;
        rows.push(record.iter().map(|field|String::from_utf8_lossy(field).into_owned()).collect());
    }
    // position() 报的是下一条未读记录的起点，按字节切分会在引号或嵌入换行处切坏记录结构。
    // 先记下续读位置，再预读一条判断是否真的还有剩余：
    // 只比较 consumed 与文件长度不可靠，末条记录后解析器位置可能仍小于总长。
    let consumed=reader.position().byte();
    let has_more=matches!(reader.byte_records().next(),Some(Ok(_)));
    Ok(AiFileSegment{
        value:serde_json::json!({
            "records":rows,
            "record_count":rows.len(),
            "next_offset":consumed,
            "truncated":has_more,
        }),
        next_offset:if has_more {consumed} else {total_len},
    })
}

/// 文本分段：按字节读取并在 UTF-8 字符边界安全截断。
/// 切到半个字符时回退到合法边界并如实上报，避免把非法字节当作内容返回。
fn ai_read_text_segment(
    mut file:std::fs::File,
    offset:u64,
    limit:u64,
    total_len:u64,
)->fluxdb_core::Result<AiFileSegment> {
    use std::io::{Read,Seek,SeekFrom};
    file.seek(SeekFrom::Start(offset)).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件偏移无效"))?;
    let mut bytes=Vec::new();
    file.take(limit).read_to_end(&mut bytes).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件读取失败"))?;
    let mut end=bytes.len();
    // 末段遇到不完整多字节序列时回退到字符边界；整段都非法则如实报错而不是有损解码。
    let text=match std::str::from_utf8(&bytes) {
        Ok(text)=>text.to_owned(),
        Err(error)=>{
            let valid=error.valid_up_to();
            if valid==0 && !bytes.is_empty() {
                return Err(fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"附件不是 UTF-8 文本或偏移切分字符，请调整范围"));
            }
            end=valid;
            String::from_utf8_lossy(&bytes[..valid]).into_owned()
        }
    };
    // 先算好位移再进 json!：宏会把 `offset+end as u64` 当成类型表达式解析。
    let next_offset=offset+end as u64;
    Ok(AiFileSegment{
        value:serde_json::json!({
            "text":text,
            "offset":offset,
            "next_offset":next_offset,
            "truncated":next_offset<total_len,
        }),
        next_offset,
    })
}

/// 授权阶段的内容类型判定：读文件头部判断是否二进制，让 file.read 能返回类型/大小而不是读取时才报错。
fn ai_file_content_summary(file:std::fs::File,label:&str)->fluxdb_core::Result<(String,bool)> {
    use std::io::Read;
    let mut head=Vec::new();
    file.take(8192).read_to_end(&mut head).map_err(|_|fluxdb_core::Error::new(fluxdb_core::ErrorKind::Internal,"无法读取附件信息"))?;
    let binary=ai_bytes_are_binary(&head);
    let kind=if label.ends_with(".csv")||label.ends_with(".tsv") {"CSV/TSV"}else if binary {"二进制"}else{"UTF-8 文本"};
    Ok((format!("{kind} · 按需分段读取"),binary))
}
