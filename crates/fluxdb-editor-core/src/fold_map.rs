//! FoldMap：把 inlay→ 行列映射为折叠后的展示行列（DM-200，Phase 2；DM-304 上游
//! 调整为 InlayPoint）。
//!
//! 显示管线中第二个真正的 [`LayerSnapshot`]：显示顺序为 Buffer → Inlay → Fold →
//! Tab → Wrap（DM-304 起在 Fold 前插入 Inlay 层）。因此本层输入 [`InlayPoint`]
//! （已含 inlay 加宽的展示列，DM-304 后），输出 [`FoldPoint`]（折叠替换后，含
//! placeholder）。折叠把内部行折叠进折叠起始行，占位符为展示行 `disp(start)` 列
//! 0 处的单个展示列槽（见 [`FOLD_PLACEHOLDER_WIDTH`]）。跨折叠边界返回
//! [`Biased`] 左右候选，调用方按 [`Bias`] 取其一。
//!
//! 本层是独立自包含展示层，仅依赖 `coordinates` / `model` / `display_map::Fold` /
//! `fold::FoldSet` / `layer::LayerSnapshot`，不依赖任何 UI 类型。折叠语义与
//! `DisplayMap` 保持一致（同样的排序、去空、合并交叉/相邻规范化；折叠起始行仍
//! 可视，仅内部行隐藏入内）。非折叠行对 InlayPoint 列恒等透传（inlay 已先加宽
//! 展示列，Fold 不再改动其列）。DisplayMap 门面保持精简组合（DM-228 起）。

use crate::coordinates::{Biased, FoldPoint, InlayPoint};
use crate::display_map::Fold;
use crate::fold::FoldSet;
use crate::layer::{LayerPatch, LayerSnapshot};
use crate::sum_tree::{IntervalSummary, SumTree, SumTreeItem, Summary};

/// 折叠占位符宽度（展示列）。
///
/// 整行折叠没有任何 buffer 列语义（内部行有自己的多列内容且被整体隐藏），因此把
/// 占位符建模为折叠起始行 `disp(start)` 列 0 处的一个 **单列槽**：left 指向占位符
/// 起点（列 0），right 指向占位符终点（列 1）。这让折叠边界处 [`Biased`] 有真实
/// 左右可区分（`is_identical()` 为假，DM-203 测试依赖）。
///
/// `ponytail:` 列 1 是合成占位符终点哨兵，不是真实 buffer 列。等字节级折叠
/// （DM-203/204）需要精确折叠列时再改为导出列；当前整行折叠用固定常量即可。
const FOLD_PLACEHOLDER_WIDTH: usize = 1;

/// 折叠展示层的不可变快照。
#[derive(Clone)]
pub struct FoldSnapshot {
    /// 绑定的输入（buffer 快照）版本号，构建时记录。
    input_version: u64,
    /// 本层自身 revision：由 input_version + fold 状态派生（FNV-1a）。
    revision: u64,
    /// 规范化折叠：排序、去空、合并交叉/相邻。`start_row` 为折叠起始（可视），
    /// `end_row` 为折叠结束（含）。
    folds: std::sync::Arc<Vec<Fold>>,
    tree: SumTree<FoldRowItem>,
}

#[derive(Clone, Debug)]
struct FoldRowItem {
    fold: Fold,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FoldRowSummary {
    start: usize,
    end: usize,
    hidden: usize,
}

impl Summary for FoldRowSummary {
    fn add(&self, other: &Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            hidden: self.hidden.saturating_add(other.hidden),
        }
    }
}

impl IntervalSummary for FoldRowSummary {
    fn start(&self) -> usize {
        self.start
    }

    fn end(&self) -> usize {
        self.end
    }
}

impl SumTreeItem for FoldRowItem {
    type Summary = FoldRowSummary;

    fn summary(&self) -> Self::Summary {
        FoldRowSummary {
            start: self.fold.start_row,
            end: self.fold.end_row,
            hidden: self.fold.end_row.saturating_sub(self.fold.start_row),
        }
    }
}

impl FoldSnapshot {
    /// 行区间构造（DisplayMap 现有的折叠形态）。内部规范化。`input_version` 取
    /// 构建时 `BufferSnapshot::version()`。
    pub fn new(input_version: u64, folds: Vec<Fold>) -> Self {
        let folds = normalize_folds(&folds);
        let revision = compute_revision(input_version, &folds);
        Self {
            input_version,
            revision,
            tree: SumTree::from_items(&row_items(&folds)),
            folds: std::sync::Arc::new(folds),
        }
    }

    /// 从稳定 [`FoldSet`] 解析构造。复用 [`FoldSet::resolve_rows`] 把稳定区间解析
    /// 为行区间 `Fold`，再走 `new` 规范化。`offset_to_row` 由调用方提供（如
    /// `|o| snapshot.offset_to_point(o).row`）。
    pub fn from_fold_set(
        input_version: u64,
        set: &FoldSet,
        offset_to_row: impl Fn(usize) -> usize,
    ) -> Self {
        let folds = set.resolve_rows(offset_to_row);
        Self::new(input_version, folds)
    }

    /// 当前规范化折叠列表（只读）。
    pub fn folds(&self) -> &[Fold] {
        self.folds.as_slice()
    }

    pub fn same_folds(&self, folds: &[Fold]) -> bool {
        normalize_folds(folds).as_slice() == self.folds.as_slice()
    }

    pub fn sync_input(&self, input_version: u64) -> Self {
        Self {
            input_version,
            revision: compute_revision(input_version, self.folds.as_slice()),
            folds: self.folds.clone(),
            tree: self.tree.clone(),
        }
    }

    /// Replace the fold set and return the affected Fold-coordinate suffix.
    ///
    /// Fold rows are prefix-summed: changing one fold can move every following
    /// display row, so the smallest correct patch starts at the first changed
    /// buffer row and ends at the old/new visible-row counts.
    pub fn sync_folds_with_patch(
        &self,
        input_version: u64,
        folds: Vec<Fold>,
        old_buffer_rows: usize,
        new_buffer_rows: usize,
    ) -> (Self, LayerPatch) {
        let normalized = normalize_folds(&folds);
        if self.folds.as_slice() == normalized.as_slice() && old_buffer_rows == new_buffer_rows {
            let next = Self {
                input_version,
                revision: compute_revision(input_version, &normalized),
                folds: std::sync::Arc::new(normalized),
                tree: self.tree.clone(),
            };
            return (next, LayerPatch::default());
        }
        let dirty_buffer_row = first_changed_fold_row(self.folds.as_slice(), &normalized);
        let first_changed = first_changed_fold_index(self.folds.as_slice(), &normalized);
        let shared_suffix = common_fold_suffix(self.folds.as_slice(), &normalized);
        let old_end = self.folds.len().saturating_sub(shared_suffix);
        let new_end = normalized.len().saturating_sub(shared_suffix);
        let tree = self.tree.replace_leaves(
            first_changed,
            old_end,
            &row_items(&normalized[first_changed..new_end]),
        );
        let next = Self {
            input_version,
            revision: compute_revision(input_version, &normalized),
            folds: std::sync::Arc::new(normalized),
            tree,
        };
        let old_start = self.display_row(dirty_buffer_row);
        let new_start = next.display_row(dirty_buffer_row);
        let old_end = visible_rows(old_buffer_rows, self.folds.as_slice());
        let new_end = visible_rows(new_buffer_rows, next.folds.as_slice());
        (
            next,
            LayerPatch::single(
                old_start..old_end.max(old_start),
                new_start..new_end.max(new_start),
            ),
        )
    }

    // ===== DM-204：四个消费端（placeholder / selection / caret / hit test）
    // ===== 共享的同一套映射 API。所有消费端只走这几个原语 + `map_input_by` /
    // ===== `map_output_by`，不再各自 `.iter().find` 重推导折叠数学。

    /// buffer 行 → 所在折叠（含起始行边界 `start_row <= row <= end_row`，折叠起始行
    /// 自身可视仍属该折叠）。placeholder / selection / caret / hit test 的 **buffer 侧**
    /// 折叠归属查询都用这一个原语（`map_input_by` 也复用它，见下）。
    fn fold_containing(&self, buffer_row: usize) -> Option<&Fold> {
        let index = self.tree.lower_bound_start(buffer_row.saturating_add(1));
        index
            .checked_sub(1)
            .and_then(|index| self.folds.get(index))
            .filter(|fold| fold.start_row <= buffer_row && buffer_row <= fold.end_row)
    }

    /// 该 buffer 行之前的隐藏行数。
    fn hidden_rows_before(&self, row: usize) -> usize {
        let index = self.tree.lower_bound_start(row);
        self.tree.summary_before_leaf(index).hidden
    }

    /// buffer 行 → 展示行号（该行与其前隐藏行的差）。
    ///
    /// 供门面侧 `row_layout`/`first_visual_row` 反向推导（DM-228 组合）。
    pub fn display_row(&self, buffer_row: usize) -> usize {
        buffer_row.saturating_sub(self.hidden_rows_before(buffer_row))
    }

    /// 展示行 → 所在折叠（该展示行是某折叠的占位符行）。折叠互不重叠且非相邻
    /// （已合并），故 `disp(start)` 在所有折叠上互不相同，可唯一确定。hit test：
    /// 「该展示行是折叠占位符行」即本查询非空。
    pub fn fold_at_display_row(&self, disp: usize) -> Option<&Fold> {
        self.folds
            .iter()
            .find(|fold| self.display_row(fold.start_row) == disp)
    }

    /// hit test：该展示行是否落在某折叠的占位符行上（要画折叠标记 / 拦截点击）。
    /// 与 [`FoldSnapshot::fold_at_display_row`] 完全相同，是 placeholder 与 hit test
    /// 共享的同一归属查询。
    pub fn is_folded_display_row(&self, disp: usize) -> bool {
        self.fold_at_display_row(disp).is_some()
    }

    /// 折叠占位符所在展示行（= 折叠起始行的展示行）。placeholder 消费端用它确定
    /// 折叠标记应绘制在哪一行。
    pub fn placeholder_display_row(&self, fold: &Fold) -> usize {
        self.display_row(fold.start_row)
    }

    /// selection 消费端：把 buffer 行区间（起止点）映射到展示区间。
    ///
    /// 起止两个端点都用同一套 `map_input_by` 逐个映射（selection 的可视化边界与
    /// placeholder/caret 用的是同一映射，保证视觉上选中区域与折叠标记/光标对齐）。
    /// 折叠把内部行折叠进占位符；若选区端点之一落在折叠内部，其展示位置即占位符
    /// 位置。返回 `(起始展示点, 结束展示点)`，排序关系由调用方（`Bias`）解释。
    ///
    /// 注意这是**端点可视位置**映射；选区实际覆盖的 buffer 内容仍由调用方持原始
    /// buffer 区间，FoldMap 只折叠端点的展示位置（折叠不改写选区语义）。
    pub fn map_input_range(
        &self,
        start: InlayPoint,
        end: InlayPoint,
    ) -> (Biased<FoldPoint>, Biased<FoldPoint>) {
        (self.map_input_by(start), self.map_input_by(end))
    }

    /// 展示行 → buffer 行（反向）。仅用于非占位符行；占位符行在
    /// `map_output_by` 中已先短路处理。
    ///
    /// 折叠互不重叠且已排序/合并，可见 buffer 行即「非折叠内部行」，展示行是可见
    /// 行的序数。因此按序扫描折叠：每段可见区间为 `[buffer ..= fold.start_row]`
    /// （折叠起始行自身可视），段内可见行逐一与目标展示行比对；越过折叠后跳过其
    /// 内部隐藏行。无需 buffer 总行数即可取值。caret / selection 的反向映射消费端
    /// 复用它。
    ///
    /// `ponytail:` 线性扫描（O(折叠数)）定位区间。折叠数通常远小于行数，可接受；
    /// 若未来折叠规模变大，可在构造时预置 SumTree 前缀和走二分。
    pub fn buffer_row_for_display(&self, disp: usize) -> usize {
        let mut buffer = 0usize; // 本段可见区起始 buffer 行
        let mut seen = 0usize; // 已计入的可见行数（展示行序数）
        for fold in self.folds.iter() {
            // 该折叠前的一段可见区 [buffer ..= fold.start_row]。
            let visible_in_segment = fold.start_row.saturating_sub(buffer) + 1;
            if seen + visible_in_segment > disp {
                return buffer + (disp - seen);
            }
            seen += visible_in_segment;
            // 越过该折叠，跳过其内部隐藏行（start+1..=end），下一段从 end+1 起。
            buffer = fold.end_row.saturating_add(1);
        }
        buffer + (disp - seen)
    }
}

impl LayerSnapshot for FoldSnapshot {
    type Input = InlayPoint;
    type Output = FoldPoint;

    fn input_version(&self) -> u64 {
        self.input_version
    }

    fn revision(&self) -> u64 {
        self.revision
    }

    fn map_input_by(&self, input: InlayPoint) -> Biased<FoldPoint> {
        let InlayPoint { row: r, column: c } = input;
        // 折叠归属与 placeholder/hit test 共享同一 `fold_containing` 原语（DM-204），
        // 不在此单独 `.iter().find`。
        if let Some(fold) = self.fold_containing(r) {
            let disp = self.placeholder_display_row(fold);
            if r == fold.start_row && c > 0 {
                return Biased::from(FoldPoint {
                    row: disp,
                    column: c,
                });
            }
            // 内部行（start < r <= end）或折叠行边界（列 0）：映射到占位符。
            return Biased {
                left: FoldPoint {
                    row: disp,
                    column: 0,
                },
                right: FoldPoint {
                    row: disp,
                    column: FOLD_PLACEHOLDER_WIDTH,
                },
            };
        }
        // 折叠外部：恒等映射。
        Biased::from(FoldPoint {
            row: self.display_row(r),
            column: c,
        })
    }

    fn map_output_by(&self, output: FoldPoint) -> Biased<InlayPoint> {
        let FoldPoint { row: o, column: c } = output;
        // 占位符行：折叠边界（列 0，占位符单列槽）反向到折叠边界，left 在折叠起始
        // 行、right 到折叠后首行（折叠非相邻/已合并，`end+1` 恒为可见间隙行）；列
        // ≥ 占位符宽度的输出列属于折叠起始行的可见内容，按列恒等反向（与
        // `map_input_by` 起始行列 >0 恒等映射互逆，保证 round-trip，DM-201）。
        if let Some(fold) = self.fold_at_display_row(o) {
            if c >= FOLD_PLACEHOLDER_WIDTH {
                return Biased::from(InlayPoint::new(fold.start_row, c));
            }
            return Biased {
                left: InlayPoint::new(fold.start_row, 0),
                right: InlayPoint::new(fold.end_row.saturating_add(1), 0),
            };
        }
        // 普通行：恒等反向。
        Biased::from(InlayPoint::new(self.buffer_row_for_display(o), c))
    }
}

/// 派生本层 revision：FNV-1a 混合 input_version 与折叠状态。
///
/// 折叠已规范化排序，故相同折叠状态 → 相同 revision；折叠或 input_version 任一
/// 变化都会翻转 revision，使 [`LayerSnapshot::is_current`]（layer.rs 默认实现）
/// 正确判定过期。revision 是缓存有效性键而非身份标识，理论哈希碰撞可接受。
fn compute_revision(input_version: u64, folds: &[Fold]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x100000001b3);
    mix(input_version);
    mix(folds.len() as u64);
    for f in folds {
        mix(f.start_row as u64);
        mix(f.end_row as u64);
    }
    h
}

fn visible_rows(buffer_rows: usize, folds: &[Fold]) -> usize {
    let hidden = folds
        .iter()
        .map(|fold| fold.end_row.saturating_sub(fold.start_row))
        .sum::<usize>();
    buffer_rows.max(1).saturating_sub(hidden).max(1)
}

fn row_items(folds: &[Fold]) -> Vec<FoldRowItem> {
    folds
        .iter()
        .copied()
        .map(|fold| FoldRowItem { fold })
        .collect()
}

fn first_changed_fold_index(old: &[Fold], new: &[Fold]) -> usize {
    old.iter()
        .zip(new.iter())
        .position(|(a, b)| a != b)
        .unwrap_or(old.len().min(new.len()))
}

fn first_changed_fold_row(old: &[Fold], new: &[Fold]) -> usize {
    old.iter()
        .zip(new.iter())
        .position(|(a, b)| a != b)
        .map_or_else(
            || {
                old.get(new.len()).map_or_else(
                    || new.get(old.len()).map_or(0, |f| f.start_row),
                    |f| f.start_row,
                )
            },
            |i| old[i].start_row.min(new[i].start_row),
        )
}

fn common_fold_suffix(old: &[Fold], new: &[Fold]) -> usize {
    let mut count = 0;
    while count < old.len()
        && count < new.len()
        && old[old.len() - 1 - count] == new[new.len() - 1 - count]
    {
        count += 1;
    }
    count
}

/// 规范化折叠（DM-203 明确政策）：按 `start_row` 排序；丢弃空/退化区间；合并
/// **交叉/嵌套/相邻（相触）** 区间。相邻区间（`last.end_row + 1 >= f.start_row`）
/// 合并，与 [`FoldSet`] 的增量插入（相触即合并、`FoldSet` 永不存相邻条目）政策一致，
/// 保证 `from_fold_set` 解析出的产物与 `FoldSet` 内部状态同构。
///
/// 这与 `DisplayMap::normalize_folds`（display_map.rs）的旧副本行为仅在「相邻是否
/// 合并」上有差异——旧副本保留相邻。DisplayMap 门面冻结至 DM-228，故在其副本处
/// 保留旧语义、不在这里改，避免破坏冻结中的 facade。
fn normalize_folds(folds: &[Fold]) -> Vec<Fold> {
    let mut out: Vec<Fold> = Vec::new();
    let mut sorted: Vec<Fold> = folds.to_vec();
    sorted.sort_by_key(|f| f.start_row);
    for f in sorted {
        if f.end_row <= f.start_row {
            continue;
        }
        if let Some(last) = out.last_mut() {
            if f.start_row <= last.end_row + 1 {
                last.end_row = last.end_row.max(f.end_row);
                continue;
            }
        }
        out.push(f);
    }
    out
}
