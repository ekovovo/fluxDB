//! 用户折叠的稳定状态（DM-106）。
//!
//! 折叠在旧实现里以「行号集合」保存（用户折叠哪些候选行），编辑发生在折叠点
//! 之前时行号会错位，可能匹配到另一个语句。本模块把折叠状态迁移为基于
//! [`AnchorRange`]（DM-105 稳定锚点区间）的唯一标识折叠，编辑后显式重定位，
//! 不再依赖行号猜测。
//!
//! `DisplayMap` 在 Phase 2 前的 facade 仍消费行区间 `Fold`（DM-108）；因此
//! `FoldSet` 在需要时以当前快照把稳定区间解析回行区间 `Fold`，保证 UI 迁移期
//! 不感知内部层变化。

use crate::display_map::Fold;
use crate::model::{AnchorRange, Bias, Range};
use crate::sum_tree::{IntervalSummary, SumTree, SumTreeItem};

/// 折叠的唯一稳定 id。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FoldId(pub u64);

/// 一个稳定折叠：内容绑定到 `AnchorRange`，编辑后重定位（DM-105）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoldEntry {
    pub id: FoldId,
    /// 折叠的字节区间（半开，两端稳定锚点）。start 用 `Bias::Left`、end 用
    /// `Bias::Right`：在边界插入时折叠保持包含原内容。
    pub range: AnchorRange,
}

/// 折叠条目在 [`FoldSet`] 内部 SumTree 中的叶子项。
///
/// 摘要携带区间包络 `[start, end)`（字节偏移）与条目数，使 add/remove/edit 只替换
/// 相交叶子区间，未触碰的子树节点全部 `Arc` 共享（持久，DM-202）。
#[derive(Clone, Debug)]
struct FoldSumItem {
    entry: FoldEntry,
}

/// [`FoldSumItem`] 的子树叶摘要：`start`/`end` 为该叶子（或子树）所有条目区间的
/// 最小起点 / 最大终点包络。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FoldSummary {
    start: usize,
    end: usize,
    count: usize,
}

impl crate::sum_tree::Summary for FoldSummary {
    fn add(&self, other: &Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            count: self.count + other.count,
        }
    }
}

impl IntervalSummary for FoldSummary {
    fn start(&self) -> usize {
        self.start
    }
    fn end(&self) -> usize {
        self.end
    }
}

impl SumTreeItem for FoldSumItem {
    type Summary = FoldSummary;

    fn summary(&self) -> Self::Summary {
        Self::Summary {
            start: self.entry.range.start.offset,
            end: self.entry.range.end.offset,
            count: 1,
        }
    }
}

/// 稳定折叠集合：按起始偏移排序、互不重叠。
///
/// 构造时以当前快照的字节区间建立（语言 `fold_ranges` / 旧行区间均可迁移），
/// 之后每次编辑调用 [`FoldSet::relocate`] 重定位，用户折叠即绑定到内容而非行号。
///
/// 条目存储于按起始偏移排序的 [`SumTree`]：add/remove/edit 只替换相交的叶子区间
/// （persistent `replace_leaves`，未触碰子树 `Arc` 共享），而不是整体重建 —— 即
/// DM-202「fold add/remove/edit 只替换相交 transform path」的稳定状态侧实现。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FoldSet {
    next_id: u64,
    tree: SumTree<FoldSumItem>,
}

impl FoldSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.tree.total().count == 0
    }

    pub fn len(&self) -> usize {
        self.tree.total().count
    }

    /// 迁移入口：从「当前快照字节偏移」的区间序列建立稳定折叠状态（DM-106）。
    ///
    /// 过滤退化区间（空 / 单字节），并去除包含关系与重叠，按起点排序。
    /// 来源可为语言 `fold_ranges` 或把旧行区间 `Fold` 解析到字节后传入。
    pub fn from_byte_ranges(ranges: impl IntoIterator<Item = Range>) -> Self {
        let mut set = FoldSet::new();
        let mut ranges: Vec<Range> = ranges.into_iter().filter(|r| !r.is_empty()).collect();
        ranges.sort_unstable_by_key(|r| (r.start, r.end));
        for r in ranges {
            set.insert_normalized(r);
        }
        set
    }

    /// 插入一个区间，规范化内部重叠/包含（含相触合并），保持排序，不重复 id 分配。
    /// 新条目总会获得一个新 [`FoldId`]。
    ///
    /// DM-202：只替换「与新区间相交（含相触）」的叶子区间 ——
    /// [`SumTree::intersecting_leaf_span`] 定位受影响的下标范围，被合并后的条目经
    /// [`SumTree::replace_leaves`] 在该区间原地替换；未触碰子树节点 `Arc` 共享
    /// （持久 path），不整体重建。无相交时是纯插入：以 `lower_bound_start` 定插入点。
    fn insert_normalized(&mut self, range: Range) {
        if range.is_empty() {
            return;
        }
        let id = FoldId(self.next_id);
        self.next_id += 1;
        let entry = FoldEntry {
            id,
            range: to_anchor_range(range),
        };
        let item = FoldSumItem { entry };
        match self.tree.intersecting_leaf_span((range.start, range.end)) {
            Some((first, last_excl)) => {
                // 与相交条目合并：取所有相交叶子的真实区间包络。
                let mut merged = range;
                for idx in first..last_excl {
                    if let Some(it) = self.tree.get(idx) {
                        let e = &it.entry;
                        merged = Range::new(
                            merged.start.min(e.range.start.offset),
                            merged.end.max(e.range.end.offset),
                        );
                    }
                }
                self.tree = self.tree.replace_leaves(
                    first,
                    last_excl,
                    &[FoldSumItem {
                        entry: FoldEntry {
                            id,
                            range: to_anchor_range(merged),
                        },
                    }],
                );
            }
            None => {
                // 纯插入：找第一个 start >= range.start 位置，原地插一个叶子。
                let idx = self.tree.lower_bound_start(range.start);
                self.tree = self.tree.replace_leaves(idx, idx, &[item]);
            }
        }
    }

    /// 切换一个字节区间对应的折叠：存在则移除，不存在则加入。
    /// 返回是否有变化。
    pub fn toggle_range(&mut self, range: Range) -> bool {
        if self.contains(range) {
            self.remove(range)
        } else {
            let before = self.len();
            self.insert_normalized(range);
            self.len() != before
        }
    }

    /// 该字节区间当前是否处于折叠状态。
    pub fn contains(&self, range: Range) -> bool {
        let idx = self.tree.lower_bound_start(range.start);
        idx < self.len()
            && self.tree.get(idx).map_or(false, |item| {
                item.entry.range.start.offset == range.start
                    && item.entry.range.end.offset == range.end
            })
    }

    /// 无条件移除一个字节区间对应的折叠（若存在）。用于「展开某折叠」类语义：
    /// 目标区间不在集合中也视为成功（幂等），不新增。返回是否发生了移除。
    ///
    /// DM-202：精确定位该条目叶子（起始偏移二分 + 校验首叶），只替换单个叶子区间。
    pub fn remove(&mut self, range: Range) -> bool {
        let idx = self.tree.lower_bound_start(range.start);
        if idx >= self.len() {
            return false;
        }
        let exact = self.tree.get(idx).map_or(false, |item| {
            item.entry.range.start.offset == range.start && item.entry.range.end.offset == range.end
        });
        if !exact {
            return false;
        }
        self.tree = self.tree.replace_leaves(idx, idx + 1, &[]);
        true
    }

    /// 按一次编辑重定位所有折叠（DM-105）。`old_range` 为被替换区间，`new_len`
    /// 为新文本长度。折叠绑定内容，编辑发生在折叠之前时不会漂移。
    ///
    /// DM-203：重定位会移动各折叠的起止，可能使原本分开的折叠变成**重叠/相邻**，
    /// 故重定位后重新规范化——丢弃退化为空的折叠、合并重叠/相邻条目并保序。合并
    /// 时保留该组最早条目的 [`FoldId`]（折叠身份不漂移；`relocate` 不分配新 id）。
    pub fn relocate(&mut self, old_range: Range, new_len: usize) {
        let items: Vec<FoldEntry> = self
            .tree
            .to_vec()
            .into_iter()
            .map(|item| item.entry)
            .collect();
        // 重定位后仍按起点升序（sum_tree 保序重定位每个条目），丢弃退化，再归并
        // 相邻/重叠。首条 id 保留、后续相邻条目的区间并入第一条。
        let mut merged: Vec<FoldSumItem> = Vec::new();
        for mut e in items {
            e.range = e.range.relocate(old_range, new_len);
            if e.range.end.offset <= e.range.start.offset {
                continue; // 塌缩为空，丢弃
            }
            if let Some(last) = merged.last_mut() {
                // 相触（end 相邻）或重叠 → 并入前一条（保持首条 id）。
                if e.range.start.offset <= last.entry.range.end.offset {
                    // 终点锚点取两者偏移较大者（原地复用已重定位的锚点，保持 bias）。
                    if e.range.end.offset > last.entry.range.end.offset {
                        last.entry.range.end = e.range.end;
                    }
                    continue;
                }
            }
            merged.push(FoldSumItem { entry: e });
        }
        // 重定位可能改变条目数（合并/塌缩），全量重建一次（relocate 罕见）。
        self.tree = SumTree::from_items(&merged);
    }

    /// 按当前快照把稳定区间解析为行区间 `Fold`，供现有 `DisplayMap` facade 消费。
    ///
    /// `offset_to_row(offset) -> 行号` 由调用方提供（如
    /// `|o| snapshot.offset_to_point(o).row`）；区间终点回退到所在行，退化为
    /// 同一行的区间被丢弃。
    pub fn resolve_rows(&self, offset_to_row: impl Fn(usize) -> usize) -> Vec<Fold> {
        self.entries()
            .into_iter()
            .filter_map(|e| {
                let start_row = offset_to_row(e.range.start.offset);
                // 终点用闭区间语义：至少覆盖到其所在行。
                let end_offset = e.range.end.offset.saturating_sub(1);
                let end_row = offset_to_row(end_offset);
                if end_row > start_row {
                    Some(Fold { start_row, end_row })
                } else {
                    None
                }
            })
            .collect()
    }

    /// 遍历稳定折叠（供 FoldMap/diagnostic 直接消费条目）。按起始偏移升序。
    pub fn iter(&self) -> impl Iterator<Item = FoldEntry> {
        self.entries().into_iter()
    }

    /// 当前稳定折叠条目（按起始偏移升序）。拷贝自 SumTree（O(n)，用于消费端）。
    pub fn entries(&self) -> Vec<FoldEntry> {
        self.tree
            .to_vec()
            .into_iter()
            .map(|item| item.entry)
            .collect()
    }
}

fn to_anchor_range(range: Range) -> AnchorRange {
    AnchorRange::new(
        crate::model::Anchor::new(range.start, Bias::Left),
        crate::model::Anchor::new(range.end, Bias::Right),
    )
}
