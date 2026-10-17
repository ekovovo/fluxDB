//! 显示层不可变 Snapshot 的统一契约（DM-103）。
//!
//! 各显示层（Inlay/Fold/Tab/Wrap/Block）在生产时最终都对外暴露一个不可变
//! Snapshot，供 UI 与下游层只读消费。本模块用 trait 固化这个共同约定，
//! 保证：
//!
//! - 每层 Snapshot 绑定输入版本（`input_version`）并持有自身 `revision`；
//! - 提供 input ↔ output 双向映射（`map_input_by` / `map_output_by`），
//!   跨零宽边界 / fold placeholder / inlay 边界时以 [`Biased`] 返回候选；
//! - 结果提交或缓存失效前用 [`LayerSnapshot::is_current`] 校验捕获的
//!   version/revision，旧版本结果不得提交（见设计 7.1）。
//!
//! 具体层（FoldMap、TabMap、WrapMap、InlayMap、BlockMap）在对应阶段实现本
//! trait 并叠加各自领域类型；此处只锁定契约语义，不预建空壳层。

use crate::coordinates::Biased;

/// 一层 transform 同步产生的 dirty edit（坐标单位由调用层约定）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerEdit {
    pub old: std::ops::Range<usize>,
    pub new: std::ops::Range<usize>,
}

impl LayerEdit {
    pub fn is_empty(&self) -> bool {
        self.old.is_empty() && self.new.is_empty()
    }

    pub fn old_len(&self) -> usize {
        self.old.len()
    }

    pub fn new_len(&self) -> usize {
        self.new.len()
    }
}

/// 各显示层共享的轻量 dirty patch 载荷。最终 UI patch 由 DisplayMap 转换为
/// `DisplayPatch`，避免底层层实现依赖 UI facade。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerPatch {
    edits: Vec<LayerEdit>,
}

impl LayerPatch {
    pub fn new(edits: Vec<LayerEdit>) -> Self {
        debug_assert!(
            edits
                .windows(2)
                .all(|pair| pair[0].old.start <= pair[1].old.start)
        );
        Self { edits }
    }

    pub fn single(old: std::ops::Range<usize>, new: std::ops::Range<usize>) -> Self {
        Self {
            edits: vec![LayerEdit { old, new }],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn edits(&self) -> &[LayerEdit] {
        &self.edits
    }

    pub fn into_edits(self) -> Vec<LayerEdit> {
        self.edits
    }

    pub fn push(&mut self, old: std::ops::Range<usize>, new: std::ops::Range<usize>) {
        let edit = LayerEdit { old, new };
        if edit.is_empty() {
            return;
        }
        self.push_maybe_empty(edit);
    }

    pub fn push_maybe_empty(&mut self, edit: LayerEdit) {
        if let Some(last) = self.edits.last_mut()
            && last.old.end >= edit.old.start
        {
            last.old.end = last.old.end.max(edit.old.end);
            last.new.end = last.new.end.max(edit.new.end);
            return;
        }
        self.edits.push(edit);
    }

    pub fn extend(&mut self, other: Self) {
        for edit in other.edits {
            self.push_maybe_empty(edit);
        }
    }

    /// Compose `self` (old -> mid) with `next` (mid -> new), preserving the
    /// coordinate-space semantics used by Zed's Patch.
    pub fn compose(&self, next: impl IntoIterator<Item = LayerEdit>) -> Self {
        let mut old_edits = self.edits.clone();
        let mut new_edits: Vec<LayerEdit> = next.into_iter().collect();
        let mut old_index = 0;
        let mut new_index = 0;
        let mut old_start = 0usize;
        let mut new_start = 0usize;
        let mut composed = Self::default();

        loop {
            let old_edit = old_edits.get(old_index).cloned();
            let new_edit = new_edits.get(new_index).cloned();

            if let Some(old_edit) = old_edit.as_ref()
                && new_edit
                    .as_ref()
                    .is_none_or(|new_edit| old_edit.new.end < new_edit.old.start)
            {
                let catchup = old_edit.old.start.saturating_sub(old_start);
                old_start += catchup;
                new_start += catchup;
                let old_end = old_start + old_edit.old_len();
                let new_end = new_start + old_edit.new_len();
                composed.push(old_start..old_end, new_start..new_end);
                old_start = old_end;
                new_start = new_end;
                old_index += 1;
                continue;
            }

            if let Some(new_edit) = new_edit.as_ref()
                && old_edit
                    .as_ref()
                    .is_none_or(|old_edit| new_edit.old.end < old_edit.new.start)
            {
                let catchup = new_edit.new.start.saturating_sub(new_start);
                old_start += catchup;
                new_start += catchup;
                let old_end = old_start + new_edit.old_len();
                let new_end = new_start + new_edit.new_len();
                composed.push(old_start..old_end, new_start..new_end);
                old_start = old_end;
                new_start = new_end;
                new_index += 1;
                continue;
            }

            let (Some(old_edit), Some(new_edit)) = (old_edit, new_edit) else {
                break;
            };

            if old_edit.new.start < new_edit.old.start {
                let catchup = old_edit.old.start.saturating_sub(old_start);
                old_start += catchup;
                new_start += catchup;
                let overshoot = new_edit.old.start - old_edit.new.start;
                let old_end = (old_start + overshoot).min(old_edit.old.end);
                let new_end = new_start + overshoot;
                composed.push(old_start..old_end, new_start..new_end);
                old_start = old_end;
                new_start = new_end;
                old_edits[old_index].old.start = old_end;
                old_edits[old_index].new.start += overshoot;
            } else {
                let catchup = new_edit.new.start.saturating_sub(new_start);
                old_start += catchup;
                new_start += catchup;
                let overshoot = old_edit.new.start - new_edit.old.start;
                let old_end = old_start + overshoot;
                let new_end = (new_start + overshoot).min(new_edit.new.end);
                composed.push(old_start..old_end, new_start..new_end);
                old_start = old_end;
                new_start = new_end;
                new_edits[new_index].old.start += overshoot;
                new_edits[new_index].new.start = new_end;
            }

            let old_edit = old_edits[old_index].clone();
            let new_edit = new_edits[new_index].clone();
            if old_edit.new.end > new_edit.old.end {
                let old_end = old_start + old_edit.old_len().min(new_edit.old_len());
                let new_end = new_start + new_edit.new_len();
                composed.push(old_start..old_end, new_start..new_end);
                old_edits[old_index].old.start = old_end;
                old_edits[old_index].new.start = new_edit.old.end;
                old_start = old_end;
                new_start = new_end;
                new_index += 1;
            } else {
                let old_end = old_start + old_edit.old_len();
                let new_end = new_start + old_edit.new_len().min(new_edit.new_len());
                composed.push(old_start..old_end, new_start..new_end);
                new_edits[new_index].old.start = old_edit.new.end;
                new_edits[new_index].new.start = new_end;
                old_start = old_end;
                new_start = new_end;
                old_index += 1;
            }
        }

        composed
    }

    pub fn old_to_new(&self, old: usize) -> usize {
        let index = match self.edits.binary_search_by(|edit| edit.old.start.cmp(&old)) {
            Ok(index) => index,
            Err(0) => return old,
            Err(index) => index - 1,
        };
        let Some(edit) = self.edits.get(index) else {
            return old;
        };
        if old >= edit.old.end {
            edit.new.end + old.saturating_sub(edit.old.end)
        } else {
            edit.new.start
        }
    }

    pub fn edit_for_old_position(&self, old: usize) -> LayerEdit {
        let index = match self.edits.binary_search_by(|edit| edit.old.start.cmp(&old)) {
            Ok(index) => index,
            Err(0) => {
                return LayerEdit {
                    old: old..old,
                    new: old..old,
                };
            }
            Err(index) => index - 1,
        };
        let Some(edit) = self.edits.get(index) else {
            return LayerEdit {
                old: old..old,
                new: old..old,
            };
        };
        if old > edit.old.end {
            let translated = edit.new.end + old.saturating_sub(edit.old.end);
            LayerEdit {
                old: old..old,
                new: translated..translated,
            }
        } else {
            edit.clone()
        }
    }

    pub fn invert(&mut self) -> &mut Self {
        for edit in &mut self.edits {
            std::mem::swap(&mut edit.old, &mut edit.new);
        }
        self.edits.sort_by_key(|edit| edit.old.start);
        self
    }

    pub fn old_bounds(&self) -> Option<std::ops::Range<usize>> {
        let first = self.edits.first()?;
        let last = self.edits.last()?;
        Some(first.old.start..last.old.end)
    }

    pub fn new_bounds(&self) -> Option<std::ops::Range<usize>> {
        let first = self.edits.first()?;
        let last = self.edits.last()?;
        Some(first.new.start..last.new.end)
    }

    pub fn row_delta(&self) -> isize {
        self.edits.iter().fold(0, |delta, edit| {
            delta + edit.new_len() as isize - edit.old_len() as isize
        })
    }

    pub fn clear(&mut self) {
        self.edits.clear();
    }
}

impl IntoIterator for LayerPatch {
    type Item = LayerEdit;
    type IntoIter = std::vec::IntoIter<LayerEdit>;

    fn into_iter(self) -> Self::IntoIter {
        self.edits.into_iter()
    }
}

impl<'a> IntoIterator for &'a LayerPatch {
    type Item = LayerEdit;
    type IntoIter = std::iter::Cloned<std::slice::Iter<'a, LayerEdit>>;

    fn into_iter(self) -> Self::IntoIter {
        self.edits.iter().cloned()
    }
}

/// 一个不可变显示层快照的通用契约。
///
/// `Input`/`Output` 是本层两侧坐标空间（例如 buffer ↔ fold、fold ↔ tab），
/// 由实现层指定为具体坐标 newtype。映射必须显式处理 [`Bias`]。
pub trait LayerSnapshot {
    /// 本层输入坐标类型。
    type Input: Copy;
    /// 本层输出坐标类型。
    type Output: Copy;

    /// 绑定的输入快照版本号（buffer version 或上游层 revision 组合）。
    fn input_version(&self) -> u64;

    /// 本层自身的 revision。任一影响 output 的变更（内容或配置）都会让
    /// 它单调递增；相同 revision 保证 output 逐位一致，用于结果提交/缓存
    /// key 校验。
    fn revision(&self) -> u64;

    /// input → output 映射。零宽 / fold placeholder / inlay 边界返回
    /// [`Biased`]，由调用方按 [`Bias`] 取候选。
    fn map_input_by(&self, input: Self::Input) -> Biased<Self::Output>;

    /// output → input 反向映射，同样返回两侧候选。
    fn map_output_by(&self, output: Self::Output) -> Biased<Self::Input>;

    /// 校验一个「此前捕获」的结果（连同当时的 version/revision）是否仍可
    /// 用于提交或复用：只有输入版本与本层 revision 都未变化才算有效，
    /// 否则必须视为过期并重建（设计 7.1「结果提交前检查 input version
    /// 和 layer revision」）。
    fn is_current(&self, input_version: u64, revision: u64) -> bool {
        self.input_version() == input_version && self.revision() == revision
    }
}
