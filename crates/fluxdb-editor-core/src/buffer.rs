//! 版本化文本 buffer、只读 snapshot、局部 edit 与 undo/redo。
//!
//! - 文本以分块 rope 存储，输入路径只做局部 edit，禁止每次全文 `to_string()`。
//! - snapshot 与 buffer 共享不可变 chunk（Arc），可安全交给后台任务。
//! - 版本号单调递增；后台任务结果必须携带 version，旧结果直接丢弃。
//! - 行索引按 edit 区间增量维护。

use std::sync::Arc;

use crate::line_index::LineIndex;
use crate::model::{Edit, Offset, Point, Range, TextChange};
use crate::sum_tree::{SumTree, TextSummary};

/// 单个 chunk 的上限字节数。
const MAX_CHUNK: usize = 1024;

/// 分块 rope：以不可变字符串为 chunk 的文本容器。
///
/// 快照共享 `Arc<str>` chunk，因此复制一个快照是 O(chunk 数) 的引用计数操作。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rope {
    // Snapshots share this vector; edits replace only the vector being mutated.
    chunks: Arc<Vec<Arc<str>>>,
    // Chunk start offsets make random point/range lookup logarithmic.
    chunk_starts: Arc<Vec<Offset>>,
    // Per-chunk summaries are aggregated by SumTree for byte/line/UTF-16 lookup.
    chunk_summaries: Arc<Vec<TextSummary>>,
    sum_tree: SumTree<TextSummary>,
    len: usize,
}

impl Default for Rope {
    fn default() -> Self {
        Self::new()
    }
}

impl Rope {
    fn new() -> Self {
        Self {
            chunks: Arc::new(Vec::new()),
            chunk_starts: Arc::new(vec![0]),
            chunk_summaries: Arc::new(Vec::new()),
            sum_tree: SumTree::default(),
            len: 0,
        }
    }

    fn from_str(text: &str) -> Self {
        let mut rope = Self::new();
        rope.push_str(text);
        rope
    }

    fn push_str(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        let base = self.len;
        let mut start = 0;
        while start < bytes {
            // 优先取 MAX_CHUNK 字节；若落在多字节字符中间，回退到上一个字符边界，
            // 保证每个 chunk 都以字符边界起止，快照切片与游标移动永不切到 UTF-8 中间。
            let mut end = (start + MAX_CHUNK).min(bytes);
            if end < bytes {
                while end > start && !text.is_char_boundary(end) {
                    end -= 1;
                }
                debug_assert!(end > start, "MAX_CHUNK 内应总能找到字符边界");
            }
            Arc::make_mut(&mut self.chunks).push(Arc::from(&text[start..end]));
            Arc::make_mut(&mut self.chunk_starts).push(base + end);
            Arc::make_mut(&mut self.chunk_summaries).push(summarize_chunk(&text[start..end]));
            start = end;
        }
        self.sum_tree = SumTree::from_summaries(&self.chunk_summaries);
        self.len += bytes;
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len
    }

    /// 返回绝对偏移所属 chunk 的 `(index, within_offset)`。
    fn chunk_index_for_offset(&self, offset: Offset) -> (usize, usize) {
        if self.chunks.is_empty() {
            return (0, 0);
        }
        let (i, _) = self.sum_tree.locate_byte(offset);
        let i = i.min(self.chunks.len() - 1);
        (i, offset.saturating_sub(self.sum_tree.byte_start(i)))
    }

    /// 回退一个字符到其起始字节偏移。`offset` 可落在字符中间（对外会把此类损坏游标安全吸附）。
    ///
    /// 语义：若光标停在字符起始边界，则返回紧邻其前一个字符的起始；若光标落在字符中间，
    /// 则返回其所处字符的起始（吸附）。因 chunk 均以字符边界起止，单个字符不会跨 chunk。
    fn prev_char_boundary(&self, offset: Offset) -> Offset {
        if offset == 0 {
            return 0;
        }
        let (i, within) = self.chunk_index_for_offset(offset);
        if within == 0 {
            // offset 落在 chunk i 起始（= 字符边界）：前一个字符属于 chunk i-1。
            debug_assert!(i > 0, "offset>0 时 within==0 意味着 i>0");
            let c = &self.chunks[i - 1];
            let outer = self.chunks[i - 1].len();
            let start = floor_boundary_before(c, outer);
            return offset - (outer - start);
        }
        let c = &self.chunks[i];
        let start = if c.is_char_boundary(within) {
            // 严格落在 chunk 内的字符边界：回退到前一个字符的起始。
            floor_boundary_before(c, within)
        } else {
            // 落在字符中间：吸附到当前字符的起始。
            floor_boundary_at_or_before(c, within)
        };
        offset - (within - start)
    }

    /// 前进一个字符到其结束字节偏移（即下一个字符的起始）。`offset` 可落在字符中间（吸附到该字符末尾）。
    fn next_char_boundary(&self, offset: Offset) -> Offset {
        if offset >= self.len {
            return self.len;
        }
        let (i, within) = self.chunk_index_for_offset(offset);
        if within == self.chunks[i].len() {
            // offset 恰在 chunk 末尾（字符边界）：其下一个字符是 chunk i+1 的首字符。
            let nxt = &self.chunks[i + 1];
            let chlen = utf8_len_at(nxt, 0);
            return offset + chlen;
        }
        let c = &self.chunks[i];
        let cs = floor_boundary_at_or_before(c, within);
        let chlen = utf8_len_at(c, cs);
        offset - within + cs + chlen
    }

    /// 把 `[start, end)` 区间写入 target。
    fn append_range_to_string(&self, range: Range, target: &mut String) {
        if range.start >= range.end {
            return;
        }
        let clamped_start = range.start.min(self.len);
        let clamped_end = range.end.min(self.len);
        let (si, soff) = self.chunk_index_for_offset(clamped_start);
        let (ei, eoff) = self.chunk_index_for_offset(clamped_end);
        for (idx, chunk) in self.chunks[si..=ei].iter().enumerate() {
            let idx = idx + si;
            let start_in = if idx == si { soff } else { 0 };
            let end_in = if idx == ei { eoff } else { chunk.len() };
            // 防御：range 端点可能来自跨快照的旧 offset（如 IME 组合期间文档已变化），
            // 落到多字节字符中间会触发切片 panic。此处吸附到字符边界再切，绝不产出半字。
            let start_in = floor_boundary_at_or_before(chunk, start_in);
            let end_in = floor_boundary_at_or_before(chunk, end_in);
            if start_in < end_in {
                target.push_str(&chunk[start_in..end_in]);
            }
        }
    }

    fn to_string_slice(&self, range: Range) -> String {
        let mut out = String::with_capacity(range.end.saturating_sub(range.start));
        self.append_range_to_string(range, &mut out);
        out
    }

    fn append_to_string(&self, target: &mut String) {
        for chunk in self.chunks.iter() {
            target.push_str(chunk);
        }
    }

    /// 计算 `[0, offset)` 区间文本的 UTF-16 长度（`offset` 需为字符边界）。
    ///
    /// 只遍历到目标 offset 所在的 chunk，不构造全文字符串，用于输入法 / 光标换算。
    fn utf16_until(&self, offset: usize) -> usize {
        if self.chunks.is_empty() {
            return 0;
        }
        let offset = offset.min(self.len);
        let (ei, raw_eoff) = self.chunk_index_for_offset(offset);
        // IME 可能暂时携带跨快照的字节 offset；切片前统一吸附到字符边界。
        let eoff = floor_boundary_at_or_before(&self.chunks[ei], raw_eoff);
        let mut total = self.sum_tree.summary_before_leaf(ei).utf16;
        if eoff > 0 {
            total += self.chunks[ei][..eoff]
                .chars()
                .map(|c| c.len_utf16() as usize)
                .sum::<usize>();
        }
        total
    }

    /// 给定文档级 UTF-16 offset，返回其对应的字符边界字节 offset；越界返回 `None`。
    ///
    /// 逐 chunk 累加 UTF-16 长度定位目标，仅在目标所在 chunk 内扫描字符，不构造全文。
    fn utf16_to_byte(&self, utf16: usize) -> Option<usize> {
        if self.chunks.is_empty() {
            return (utf16 == 0).then_some(0);
        }
        if utf16 > self.sum_tree.total().utf16 {
            return None;
        }
        let (i, within) = self.sum_tree.locate_utf16(utf16);
        let mut byte = 0usize;
        let mut count = 0usize;
        for c in self.chunks[i].chars() {
            if count >= within {
                break;
            }
            count += c.len_utf16();
            byte += c.len_utf8();
        }
        Some(self.chunk_starts[i] + byte)
    }

    /// 用 `new_text` 替换 `old_range` 区间，返回被替换的旧文本。
    ///
    /// 做到「真局部编辑」：只重建被编辑区间跨越的 chunk 段（起点 chunk 的前缀 +
    /// `new_text` + 终点 chunk 的后缀），起点之前与终点之后的 chunk 通过 `Arc` 共享原样保留，
    /// 不再像旧实现那样把整个后缀 `append_range_to_string` 全文拷贝。
    fn replace(&mut self, old_range: Range, new_text: &str) -> String {
        let old_len = self.len;
        let old_text = self.to_string_slice(old_range);
        if self.chunks.is_empty() {
            self.push_str(new_text);
            return old_text;
        }
        let start = old_range.start.min(old_len);
        let end = old_range.end.min(old_len).max(start);
        let (si, soff) = self.chunk_index_for_offset(start);
        let (ei, eoff) = self.chunk_index_for_offset(end);

        // 防御：`old_range` 端点可能来自跨快照的旧 offset（如 IME 组合期间文档已变化），
        // 落到多字节字符中间会触发切片 panic。此处吸附到字符边界再切，绝不产出半字。
        let soff = floor_boundary_at_or_before(&self.chunks[si], soff);
        let eoff = floor_boundary_at_or_before(&self.chunks[ei], eoff);

        // 起点之前的所有 chunk 原样共享。
        let mut kept: Vec<Arc<str>> = self.chunks[..si].to_vec();

        // 拼接被编辑区间跨越的段：起点 chunk 前缀 + new_text + 终点 chunk 后缀。
        let mut splice = String::with_capacity(new_text.len() + MAX_CHUNK * 2);
        if soff > 0 {
            splice.push_str(&self.chunks[si][..soff]);
        }
        splice.push_str(new_text);
        if ei < self.chunks.len() && eoff < self.chunks[ei].len() {
            splice.push_str(&self.chunks[ei][eoff..]);
        }
        // 分块追加（字符边界安全）。
        let mut splice_start = 0;
        let mut local_summaries = Vec::new();
        let bytes = splice.len();
        while splice_start < bytes {
            let mut splice_end = (splice_start + MAX_CHUNK).min(bytes);
            if splice_end < bytes {
                while splice_end > splice_start && !splice.is_char_boundary(splice_end) {
                    splice_end -= 1;
                }
            }
            let local_chunk = &splice[splice_start..splice_end];
            kept.push(Arc::from(local_chunk));
            local_summaries.push(summarize_chunk(local_chunk));
            splice_start = splice_end;
        }
        // 终点之后的 chunk 原样共享，不做全文拷贝。
        kept.extend_from_slice(&self.chunks[ei + 1..]);

        // 索引只拼接受影响的局部段；后缀 chunk 的字节起点整体平移。
        let byte_delta = signed_delta(new_text.len(), end - start);
        let new_len = old_len.saturating_sub(end - start) + new_text.len();
        let local_chunk_count = kept
            .len()
            .saturating_sub(si + self.chunks.len().saturating_sub(ei + 1));
        let mut starts = Vec::with_capacity(kept.len() + 1);
        starts.extend_from_slice(&self.chunk_starts[..si]);
        let mut offset = self.chunk_starts[si];
        for chunk in kept.iter().skip(si).take(local_chunk_count) {
            starts.push(offset);
            offset += chunk.len();
        }
        for old_index in (ei + 1)..self.chunks.len() {
            starts.push(shifted_offset(self.chunk_starts[old_index], byte_delta));
        }
        starts.push(new_len);

        debug_assert_eq!(starts.len(), kept.len() + 1);
        self.chunks = Arc::new(kept);
        self.chunk_starts = Arc::new(starts);
        let mut summaries = self.chunk_summaries[..si].to_vec();
        summaries.extend_from_slice(&local_summaries);
        summaries.extend_from_slice(&self.chunk_summaries[ei + 1..]);
        self.chunk_summaries = Arc::new(summaries);
        self.sum_tree = self.sum_tree.replace_leaves(si, ei + 1, &local_summaries);
        self.len = new_len;
        old_text
    }
}

/// 一段已完成的编辑，用于 undo/redo。
#[derive(Clone, Debug)]
struct EditRecord {
    /// undo 时用 old_text 替换的新区间。
    new_range: Range,
    old_text: String,
    new_text: String,
}

/// 一个 undo/redo 事务。
#[derive(Clone, Debug)]
struct EditTransaction {
    records: Vec<EditRecord>,
    mergeable: bool,
    before_anchor: Offset,
    before_cursor: Offset,
    after_anchor: Offset,
    after_cursor: Offset,
}

/// 版本化编辑器 buffer。
#[derive(Debug)]
pub struct EditorBuffer {
    text: Rope,
    version: u64,
    line_starts: LineIndex,
    undo_stack: Vec<EditTransaction>,
    redo_stack: Vec<EditTransaction>,
    undo_limit: usize,
}

impl Default for EditorBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self::new_from("")
    }

    pub fn new_from(text: &str) -> Self {
        let mut buffer = Self {
            text: Rope::from_str(text),
            version: 0,
            line_starts: LineIndex::from_offsets(&[0]),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_limit: 100,
        };
        buffer.rebuild_line_starts();
        buffer
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// 当前全文（仅测试或宿主确实需要时使用）。
    pub fn to_string(&self) -> String {
        let mut s = String::with_capacity(self.text.len());
        self.text.append_to_string(&mut s);
        s
    }

    pub fn text_in_range(&self, range: Range) -> String {
        self.text.to_string_slice(range)
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Materialize line starts when a caller needs the complete index; cursor paths query the tree.
    pub fn line_starts(&self) -> Vec<Offset> {
        self.line_starts.collect()
    }

    fn line_start_offset(&self, row: usize) -> Offset {
        self.line_starts.offset_at(row)
    }

    fn line_of_offset(&self, offset: Offset) -> usize {
        let clamped = offset.min(self.text.len());
        self.line_starts.row_for_offset(clamped)
    }

    pub fn offset_to_point(&self, offset: Offset) -> Point {
        let clamped = offset.min(self.text.len());
        let row = self.line_of_offset(clamped);
        Point::new(row, clamped - self.line_start_offset(row))
    }

    pub fn point_to_offset(&self, point: Point) -> Offset {
        let row = point.row.min(self.line_starts.len().saturating_sub(1));
        let line_start = self.line_start_offset(row);
        (line_start + point.column).min(self.text.len())
    }

    /// 回退一个字符的起始字节偏移（字符边界安全）。
    pub fn prev_char_boundary(&self, offset: Offset) -> Offset {
        self.text.prev_char_boundary(offset.min(self.text.len()))
    }

    /// 前进一个字符的结束字节偏移（字符边界安全）。
    pub fn next_char_boundary(&self, offset: Offset) -> Offset {
        self.text.next_char_boundary(offset.min(self.text.len()))
    }

    /// 返回 `<= offset` 的最大字符边界；若 `offset` 已是字符边界则原样返回（跨快照吸附用）。
    pub fn clamp_to_char_boundary(&self, offset: Offset) -> Offset {
        self.snapshot().clamp_to_char_boundary(offset)
    }

    /// 第 `row` 行文本（不含换行符）。
    pub fn line_text(&self, row: usize) -> String {
        let start = self.line_start_offset(row);
        let raw_end = if row + 1 < self.line_starts.len() {
            self.line_starts.offset_at(row + 1)
        } else {
            self.text.len()
        };
        let end = strip_trailing_newline(&self.text, raw_end);
        self.text.to_string_slice(Range::new(start, end))
    }
    /// 返回第 `row` 行的行尾偏移（含换行符）。若末行无换行则返回文本末尾。
    pub fn line_end_offset(&self, row: usize) -> Offset {
        if row + 1 < self.line_starts.len() {
            self.line_starts.offset_at(row + 1)
        } else {
            self.text.len()
        }
    }

    /// 返回第 `row` 行的起始偏移。`row` 超出范围时收敛到末行。
    pub fn line_start(&self, row: usize) -> Offset {
        self.line_start_offset(row.min(self.line_starts.len().saturating_sub(1)))
    }

    fn rebuild_line_starts(&mut self) {
        let mut line_starts = Vec::with_capacity(1024);
        line_starts.push(0);
        let mut offset = 0;
        for chunk in self.text.chunks.iter() {
            for (i, b) in chunk.bytes().enumerate() {
                if b == b'\n' {
                    line_starts.push(offset + i + 1);
                }
            }
            offset += chunk.len();
        }
        self.line_starts = LineIndex::from_offsets(&line_starts);
    }

    /// 执行一次替换。
    ///
    /// `new_cursor`/`new_anchor` 为宿主期望的编辑后光标/锚点位置（按新文本偏移给出）。
    /// `merge` 为 true 时把本次变更并入上一个事务，使连续输入可一次撤销。
    pub fn edit(
        &mut self,
        old_range: Range,
        new_text: &str,
        new_cursor: Offset,
        new_anchor: Offset,
        merge: bool,
    ) -> Edit {
        let old_range = old_range.sorted();
        self.edit_with_selection(
            old_range,
            new_text,
            new_cursor,
            new_anchor,
            old_range.start,
            old_range.end,
            merge,
        )
    }

    /// 执行替换并记录编辑前后的选区，供 undo/redo 恢复光标位置。
    pub fn edit_with_selection(
        &mut self,
        old_range: Range,
        new_text: &str,
        new_cursor: Offset,
        new_anchor: Offset,
        before_anchor: Offset,
        before_cursor: Offset,
        merge: bool,
    ) -> Edit {
        let old_range = old_range.sorted();
        // 应用文本替换，拿到被替换的旧文本。
        let old_text = self.text.replace(old_range, new_text);
        // 增量更新行起始索引。
        self.update_line_starts(old_range, new_text);

        // 记录 undo 事务。
        let record = EditRecord {
            new_range: Range::new(old_range.start, old_range.start + new_text.len()),
            old_text,
            new_text: new_text.to_string(),
        };
        let can_merge = merge
            && self
                .undo_stack
                .last()
                .map(|top| top.mergeable)
                .unwrap_or(false);
        if can_merge {
            if let Some(top) = self.undo_stack.last_mut() {
                top.records.push(record);
                top.after_anchor = new_anchor;
                top.after_cursor = new_cursor;
            }
        } else {
            self.undo_stack.push(EditTransaction {
                records: vec![record],
                mergeable: merge,
                before_anchor,
                before_cursor,
                after_anchor: new_anchor,
                after_cursor: new_cursor,
            });
        }

        self.finish_edit();
        Edit {
            // 返回本 edit 的增量变更（供宿主映射 Changed 事件）。
            changes: vec![TextChange {
                old_range,
                new_text: new_text.to_string(),
                version: self.version,
                full_document: false,
            }],
            cursor: new_cursor,
            anchor: new_anchor,
        }
    }

    fn finish_edit(&mut self) {
        if self.undo_stack.len() > self.undo_limit {
            self.undo_stack.remove(0);
        }
        if !self.redo_stack.is_empty() {
            self.redo_stack.clear();
        }
        self.version += 1;
    }

    /// 增量更新行起始索引：受影响行之前保持不变，之后重算。
    fn update_line_starts(&mut self, old_range: Range, new_text: &str) {
        self.line_starts.splice(old_range, new_text);
    }

    /// 撤销最近一次事务。
    pub fn undo(&mut self) -> Option<(Offset, Offset)> {
        let tx = self.undo_stack.pop()?;
        let selection = (tx.before_anchor, tx.before_cursor);
        for rec in tx.records.iter().rev() {
            let new_range = Range::new(
                rec.new_range.start,
                rec.new_range.start + rec.new_text.len(),
            );
            self.text.replace(new_range, &rec.old_text);
        }
        self.rebuild_line_starts();
        self.version += 1;
        self.redo_stack.push(tx);
        Some(selection)
    }

    /// 重做最近一次撤销。
    pub fn redo(&mut self) -> Option<(Offset, Offset)> {
        let tx = self.redo_stack.pop()?;
        let selection = (tx.after_anchor, tx.after_cursor);
        for rec in &tx.records {
            self.text.replace(rec.new_range, &rec.new_text);
        }
        self.rebuild_line_starts();
        self.version += 1;
        self.undo_stack.push(tx);
        Some(selection)
    }

    /// UTF-16 列。
    pub fn utf16_column_at(&self, point: Point) -> usize {
        let line = self.text.to_string_slice(Range::new(
            self.line_start_offset(point.row),
            self.point_to_offset(point),
        ));
        count_utf16(&line)
    }

    /// 当前快照（共享 chunk，后台安全）。
    pub fn snapshot(&self) -> BufferSnapshot {
        BufferSnapshot {
            rope: self.text.clone(),
            version: self.version,
            line_starts: self.line_starts.clone(),
        }
    }
}

/// 去掉行尾换行符（含 `\r\n`），使行内容不把 `\r` 计入显示列。
fn strip_trailing_newline(rope: &Rope, end: usize) -> usize {
    let mut end = end;
    if end > 0 && rope.to_string_slice(Range::new(end - 1, end)) == "\n" {
        end -= 1;
    }
    if end > 0 && rope.to_string_slice(Range::new(end - 1, end)) == "\r" {
        end -= 1;
    }
    end
}

fn count_utf16(s: &str) -> usize {
    s.chars().map(|c| c.len_utf16()).sum()
}

fn summarize_chunk(text: &str) -> TextSummary {
    TextSummary {
        bytes: text.len(),
        lines: text.bytes().filter(|byte| *byte == b'\n').count(),
        utf16: count_utf16(text),
    }
}

fn shifted_offset(offset: Offset, delta: isize) -> Offset {
    if delta >= 0 {
        offset.saturating_add(delta as usize)
    } else {
        offset.saturating_sub((-delta) as usize)
    }
}

fn signed_delta(new_value: usize, old_value: usize) -> isize {
    if new_value >= old_value {
        (new_value - old_value) as isize
    } else {
        -((old_value - new_value) as isize)
    }
}

/// `bytes[..upper)` 内最后一个字符边界（strictly < `upper`）。`upper>0` 时必有解。
fn floor_boundary_before(bytes: &str, upper: usize) -> usize {
    debug_assert!(upper > 0);
    (0..upper)
        .rev()
        .find(|&i| bytes.is_char_boundary(i))
        .unwrap_or(0)
}

/// `bytes` 内 `<= idx` 的最大字符边界。
fn floor_boundary_at_or_before(bytes: &str, idx: usize) -> usize {
    let idx = idx.min(bytes.len());
    let mut i = idx;
    while i > 0 && !bytes.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `bytes` 从 `cs`（必为字符边界）起始的第一个字符的 UTF-8 字节长。
fn utf8_len_at(bytes: &str, cs: usize) -> usize {
    bytes[cs..]
        .chars()
        .next()
        .map(|c| c.len_utf8())
        .unwrap_or(0)
}

/// 只读 buffer 快照。克隆廉价，可安全交给后台任务。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferSnapshot {
    rope: Rope,
    version: u64,
    line_starts: LineIndex,
}

impl BufferSnapshot {
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn len(&self) -> usize {
        self.rope.len()
    }

    /// 文档摘要：bytes、换行数和 UTF-16 码元数均由 SumTree 聚合得到。
    /// `lines` 是换行符数量；逻辑行数仍为 `lines + 1`（空文档也有一行）。
    pub fn summary(&self) -> TextSummary {
        self.rope.sum_tree.total()
    }

    pub fn is_empty(&self) -> bool {
        self.rope.is_empty()
    }

    pub fn to_string(&self) -> String {
        let mut s = String::with_capacity(self.rope.len());
        self.rope.append_to_string(&mut s);
        s
    }

    pub fn text_in_range(&self, range: Range) -> String {
        self.rope.to_string_slice(range)
    }

    /// 返回从精确字节 offset 开始的当前 Rope chunk 只读字节片段。
    ///
    /// 解析器可能从 UTF-8 字符中间请求输入，因此这里不能吸附字符边界；
    /// 只返回当前 chunk 的原始字节，不拼接也不复制后续文本。
    pub fn text_chunk_bytes_at(&self, offset: Offset) -> &[u8] {
        if offset >= self.rope.len() || self.rope.chunks.is_empty() {
            return &[];
        }
        let (index, within) = self.rope.chunk_index_for_offset(offset);
        self.rope.chunks[index]
            .as_bytes()
            .get(within..)
            .unwrap_or(&[])
    }

    /// 读取单个 UTF-8 原始字节，不创建临时字符串。
    pub fn byte_at(&self, offset: Offset) -> Option<u8> {
        self.text_chunk_bytes_at(offset).first().copied()
    }

    /// 计算范围内 UTF-16 码元数量，不拼接中间字符串。
    pub fn utf16_len_in_range(&self, range: Range) -> usize {
        self.text_chunks_in_range(range)
            .filter_map(|chunk| std::str::from_utf8(chunk).ok())
            .map(|text| text.chars().map(char::len_utf16).sum::<usize>())
            .sum()
    }

    /// 以零拷贝方式遍历指定字节范围覆盖的 Rope chunks。
    ///
    /// 迭代器只借用快照和底层 `Arc<str>`，不会把跨 chunk 文本拼成新的 `String`；
    /// Tree-sitter 的 `TextProvider` 可直接使用它作为节点文本来源。
    pub fn text_chunks_in_range(&self, range: Range) -> SnapshotChunkIter<'_> {
        let start = range.start.min(self.rope.len());
        let end = range.end.min(self.rope.len()).max(start);
        let first = if start < self.rope.len() {
            self.rope.chunk_index_for_offset(start).0
        } else {
            self.rope.chunks.len()
        };
        SnapshotChunkIter {
            chunks: &self.rope.chunks,
            starts: &self.rope.chunk_starts,
            index: first,
            start,
            end,
        }
    }

    pub fn offset_to_point(&self, offset: Offset) -> Point {
        let clamped = offset.min(self.rope.len());
        let row = self.line_starts.row_for_offset(clamped);
        let line_start = self.line_starts.offset_at(row);
        Point::new(row, clamped - line_start)
    }

    pub fn point_to_offset(&self, point: Point) -> Offset {
        let row = point.row.min(self.line_starts.len().saturating_sub(1));
        let line_start = self.line_starts.offset_at(row);
        (line_start + point.column).min(self.rope.len())
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Materialize line starts when a caller needs the complete index; cursor paths query the tree.
    pub fn line_starts(&self) -> Vec<Offset> {
        self.line_starts.collect()
    }

    pub fn line_start(&self, row: usize) -> Offset {
        self.line_starts.offset_at(row)
    }

    /// 第 `row` 行的行尾偏移（含换行符）；末行无换行则返回文本末尾。
    pub fn line_end_offset(&self, row: usize) -> Offset {
        if row + 1 < self.line_starts.len() {
            self.line_starts.offset_at(row + 1)
        } else {
            self.rope.len()
        }
    }

    /// 第 `row` 行文本（不含换行符）。
    pub fn line_text(&self, row: usize) -> String {
        let start = self.line_start(row);
        let raw_end = if row + 1 < self.line_starts.len() {
            self.line_starts.offset_at(row + 1)
        } else {
            self.rope.len()
        };
        let end = strip_trailing_newline(&self.rope, raw_end);
        self.text_in_range(Range::new(start, end))
    }

    pub fn utf16_column_at(&self, point: Point) -> usize {
        let line = self.text_in_range(Range::new(
            self.line_start(point.row),
            self.point_to_offset(point),
        ));
        count_utf16(&line)
    }

    /// 文档级字节 offset → 文档级 UTF-16 offset（`offset` 需为字符边界）。
    ///
    /// 只遍历到目标行 / chunk，不构造全文字符串；供输入法（IME）的 UTF-16 换算使用。
    pub fn byte_to_utf16(&self, offset: usize) -> usize {
        self.rope.utf16_until(offset)
    }

    /// 文档级 UTF-16 offset → 字符边界字节 offset；越界返回 `None`。
    pub fn utf16_to_byte(&self, utf16: usize) -> Option<Offset> {
        self.rope.utf16_to_byte(utf16)
    }

    /// 沿文本方向推进一个字节偏移，遇到非 char 边界时对齐。
    pub fn clamp_offset(&self, offset: Offset) -> Offset {
        offset.min(self.rope.len())
    }

    /// 返回 `<= offset` 的最大字符边界；若 `offset` 已是字符边界则原样返回。
    ///
    /// 用于把「跨快照的旧字节 offset」（如 IME 组合期间文档已变化而保留的 marked 区间）
    /// 吸附到当前文档的合法位置，避免落到多字节字符中间触发切片 panic。
    pub fn clamp_to_char_boundary(&self, offset: Offset) -> Offset {
        let offset = offset.min(self.rope.len());
        if self.rope.len() == 0 {
            return 0;
        }
        let (i, within) = self.rope.chunk_index_for_offset(offset);
        let snapped = floor_boundary_at_or_before(&self.rope.chunks[i], within);
        // 还原为绝对 offset：`within` 是相对 chunk 的偏移，`offset - within` 是 chunk 起始绝对偏移。
        offset - within + snapped
    }

    /// 回退一个字符的起始字节偏移（字符边界安全）。
    pub fn prev_char_boundary(&self, offset: Offset) -> Offset {
        self.rope.prev_char_boundary(offset.min(self.rope.len()))
    }

    /// 前进一个字符的结束字节偏移（字符边界安全）。
    pub fn next_char_boundary(&self, offset: Offset) -> Offset {
        self.rope.next_char_boundary(offset.min(self.rope.len()))
    }
}

/// `BufferSnapshot` 范围的零拷贝 chunk 迭代器。
pub struct SnapshotChunkIter<'a> {
    chunks: &'a [Arc<str>],
    starts: &'a [Offset],
    index: usize,
    start: Offset,
    end: Offset,
}

impl<'a> Iterator for SnapshotChunkIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        while self.index < self.chunks.len() {
            let index = self.index;
            self.index += 1;
            let chunk_start = self.starts[index];
            let chunk_end = self.starts[index + 1];
            if chunk_end <= self.start || chunk_start >= self.end {
                continue;
            }
            let start = floor_boundary_at_or_before(
                &self.chunks[index],
                self.start
                    .saturating_sub(chunk_start)
                    .min(self.chunks[index].len()),
            );
            let end = floor_boundary_at_or_before(
                &self.chunks[index],
                self.end
                    .saturating_sub(chunk_start)
                    .min(self.chunks[index].len()),
            );
            if start < end {
                return Some(&self.chunks[index].as_bytes()[start..end]);
            }
        }
        None
    }
}

#[cfg(test)]
mod line_edit_tests {
    use super::*;

    #[test]
    fn successive_edits_preserve_shifted_line_starts_and_snapshots() {
        for (first_range, first_text, first_starts, expected_starts, expected_lines) in [
            (
                Range::empty(0),
                "x",
                vec![0, 4, 7, 10, 13],
                vec![0, 4, 8, 11, 14],
                ["xaa", "ybb", "cc", "dd", ""],
            ),
            (
                Range::new(0, 1),
                "",
                vec![0, 2, 5, 8, 11],
                vec![0, 2, 6, 9, 12],
                ["a", "ybb", "cc", "dd", ""],
            ),
        ] {
            let mut buffer = EditorBuffer::new_from("aa\nbb\ncc\ndd\n");
            buffer.edit(
                first_range,
                first_text,
                first_text.len(),
                first_text.len(),
                false,
            );
            let snapshot = buffer.snapshot();
            assert_eq!(snapshot.line_starts(), first_starts);

            let offset = buffer.line_start(1);
            buffer.edit(Range::empty(offset), "y", offset + 1, offset + 1, false);
            assert_eq!(buffer.line_starts(), expected_starts);
            for (row, expected) in expected_lines.iter().enumerate() {
                assert_eq!(buffer.line_text(row), *expected);
                assert_eq!(buffer.snapshot().line_text(row), *expected);
            }
            assert_eq!(snapshot.line_starts(), first_starts);
            assert_eq!(snapshot.line_text(1), "bb");
        }
    }
}
