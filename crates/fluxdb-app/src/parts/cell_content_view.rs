// 超大字段的分段只读视图（详情面板用）。
//
// 完整内容在受控内容存储里（`cell_content_store`），这里只保留**有上限的窗口**：
// 按块读取、按字节偏移记录，超出 [`CellDetailCache`] 上限就淘汰最旧的块。
// 关键约束：
// - 每块都是同一份不可变内容的一段，不从数据库重查；
// - 块边界按 UTF-8 字符边界对齐，`offset` 始终是原始字节偏移；
// - 预览（`LargeCellValue::preview`）不能当完整值，复制/保存必须走内容引用。

/// 详情面板连续缓存上限：超过就淘汰最旧的块，不能无限追加。
pub const CELL_DETAIL_CACHE_LIMIT_BYTES: u64 = fluxdb_core::CELL_DETAIL_CACHE_BYTES;

#[derive(Clone, Debug, PartialEq)]
pub struct CellContentChunk {
    /// 块在完整内容中的起始字节偏移。
    pub offset: u64,
    /// 已按 UTF-8 边界解码的文本；二进制块是 hex。
    pub text: String,
    /// 块覆盖的原始字节数。
    pub byte_length: u64,
}

impl CellContentChunk {
    pub fn end_offset(&self) -> u64 {
        self.offset.saturating_add(self.byte_length)
    }
}

/// 详情面板里的大字段视图状态。
#[derive(Clone, Debug, PartialEq)]
pub struct LargeValueView {
    pub content_id: String,
    pub kind: fluxdb_core::LargeValueKind,
    /// 完整内容大小；库端/存储拿不到时为 `None`。
    pub byte_length: Option<u64>,
    /// 按 offset 升序、互不重叠的窗口。
    pub chunks: Vec<CellContentChunk>,
    pub loading: bool,
    /// 已读到内容末尾：有大小且窗口覆盖到末尾，或读到空块（大小未知时唯一的终点信号）。
    pub reached_end: bool,
    pub error: Option<String>,
}

impl LargeValueView {
    pub fn new(large: &fluxdb_core::LargeCellValue) -> Self {
        Self {
            content_id: large.content_id.clone(),
            kind: large.kind,
            byte_length: large.byte_length,
            chunks: Vec::new(),
            loading: false,
            reached_end: false,
            error: None,
        }
    }

    /// 窗口覆盖的字节区间 `[start, end)`；空窗口返回 `None`。
    pub fn window(&self) -> Option<(u64, u64)> {
        let first = self.chunks.first()?;
        let last = self.chunks.last()?;
        Some((first.offset, last.end_offset()))
    }

    pub fn cached_bytes(&self) -> u64 {
        self.chunks.iter().map(|chunk| chunk.byte_length).sum()
    }

    /// 是否已读到内容末尾。大小未知时只能靠空块确认，不能瞎猜成完整。
    pub fn is_complete(&self) -> bool {
        if self.reached_end {
            return true;
        }
        match (self.byte_length, self.window()) {
            (Some(length), Some((_, end))) => end >= length,
            _ => false,
        }
    }

    /// 下一块从哪里读：有窗口就接在窗口末尾，否则从头开始。
    pub fn next_read_offset(&self) -> u64 {
        self.window().map(|(_, end)| end).unwrap_or(0)
    }

    /// 追加一块并按上限淘汰最旧的块。
    pub fn push_chunk(&mut self, chunk: CellContentChunk) {
        if chunk.byte_length == 0 {
            self.reached_end = true;
            return;
        }
        self.chunks.push(chunk);
        // 淘汰最旧块：可用字节数有上限，而不是让用户一直点下去把内存撑爆。
        while self.cached_bytes() > CELL_DETAIL_CACHE_LIMIT_BYTES && self.chunks.len() > 1 {
            self.chunks.remove(0);
        }
    }

    /// 窗口文本（按块顺序拼接）。
    pub fn window_text(&self) -> String {
        let mut text = String::new();
        for chunk in &self.chunks {
            text.push_str(&chunk.text);
        }
        text
    }

    /// 完整内容大小；未知时如实显示 `?`，不能拿窗口长度冒充。
    pub fn size_label(&self) -> String {
        self.byte_length
            .map(fluxdb_core::format_byte_length)
            .unwrap_or_else(|| "?".to_string())
    }

    /// 用户可见的状态说明：当前范围 / 总大小。
    pub fn range_label(&self) -> String {
        let total = self.size_label();
        match self.window() {
            Some((start, end)) => format!(
                "{} – {} / {}",
                fluxdb_core::format_byte_length(start),
                fluxdb_core::format_byte_length(end),
                total
            ),
            None => format!("0 / {total}"),
        }
    }
}

/// 把一段原始字节切成可显示的块：文本按 UTF-8 字符边界回退，二进制转 hex。
///
/// 返回 `(text, 实际覆盖字节数)`。末尾不完整的多字节字符**不**计入本块，
/// 留给下一块从它的首字节继续，从而保证每块的 `offset` 都落在字符边界上。
pub fn decode_content_chunk(kind: fluxdb_core::LargeValueKind, bytes: &[u8]) -> (String, u64) {
    if kind == fluxdb_core::LargeValueKind::Binary {
        let hex = bytes.iter().map(|byte| format!("{byte:02X}")).collect::<String>();
        return (hex, bytes.len() as u64);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), bytes.len() as u64),
        Err(error) => {
            let valid = error.valid_up_to();
            // 只有「结尾截断在多字节字符中间」才能安全回退；
            // 中间的非法字节说明内容本身不是合法 UTF-8，用替换字符如实展示。
            if error.error_len().is_none() {
                let text = String::from_utf8_lossy(&bytes[..valid]).into_owned();
                (text, valid as u64)
            } else {
                (String::from_utf8_lossy(bytes).into_owned(), bytes.len() as u64)
            }
        }
    }
}

#[cfg(test)]
mod large_value_view_tests {
    use super::*;

    fn chunk(offset: u64, text: &str, byte_length: u64) -> CellContentChunk {
        CellContentChunk {
            offset,
            text: text.to_string(),
            byte_length,
        }
    }

    fn view(byte_length: Option<u64>) -> LargeValueView {
        LargeValueView {
            content_id: "c1".into(),
            kind: fluxdb_core::LargeValueKind::Text,
            byte_length,
            chunks: Vec::new(),
            loading: false,
            reached_end: false,
            error: None,
        }
    }

    #[test]
    fn chunks_concatenate_to_window_text() {
        let mut view = view(Some(6));
        view.push_chunk(chunk(0, "abc", 3));
        view.push_chunk(chunk(3, "def", 3));
        assert_eq!(view.window_text(), "abcdef");
        assert_eq!(view.window(), Some((0, 6)));
        assert!(view.is_complete());
    }

    #[test]
    fn oldest_chunks_are_evicted_at_cache_limit() {
        let mut view = view(Some(u64::MAX));
        let half = CELL_DETAIL_CACHE_LIMIT_BYTES / 2 + 1;
        view.push_chunk(chunk(0, "a", half));
        view.push_chunk(chunk(half, "b", half));
        assert_eq!(view.chunks.len(), 1, "先入的块必须被淘汰");
        assert_eq!(view.window(), Some((half, half * 2)));
        assert!(view.cached_bytes() <= CELL_DETAIL_CACHE_LIMIT_BYTES);
    }

    #[test]
    fn truncated_multibyte_char_moves_to_next_chunk() {
        let source = "汉".as_bytes(); // 3 字节
        let (text, consumed) = decode_content_chunk(fluxdb_core::LargeValueKind::Text, &source[..2]);
        assert_eq!(text, "");
        assert_eq!(consumed, 0, "半个字符不能计入本块");
        let (text, consumed) = decode_content_chunk(fluxdb_core::LargeValueKind::Text, source);
        assert_eq!(text, "汉");
        assert_eq!(consumed, 3);
    }

    #[test]
    fn binary_chunk_decodes_as_hex() {
        let (text, consumed) = decode_content_chunk(fluxdb_core::LargeValueKind::Binary, &[0xAB, 0x01]);
        assert_eq!(text, "AB01");
        assert_eq!(consumed, 2);
    }

    #[test]
    fn unknown_size_is_never_reported_complete() {
        let mut view = view(None);
        view.push_chunk(chunk(0, "abc", 3));
        assert!(!view.is_complete());
        assert_eq!(view.range_label(), "0 B – 3 B / ?");
    }

    #[test]
    fn empty_chunk_marks_end_when_size_unknown() {
        let mut view = view(None);
        view.push_chunk(chunk(0, "abc", 3));
        view.push_chunk(chunk(3, "", 0));
        assert!(view.is_complete());
        assert_eq!(view.window(), Some((0, 3)), "空块不进窗口");
    }
}
