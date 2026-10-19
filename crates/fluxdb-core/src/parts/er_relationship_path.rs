// ER 连接路径搜索（er-design §6.6 查询服务地基）。
//
// 纯图算法：不读数据库、不查结构快照。宿主先把「当前判定为可用的关系」连同其
// `ErJoinPlan` 交给本函数，本函数只在这些关系组成的图上做有界枚举。搜索上限由
// 这里的常量固定，调用方只能在更小的范围内收紧，不能放宽：
//
// - 路径不重复经过同一张表（不含环）；自关联关系（同表两端）不参与跨表路径；
// - 扩展次数与候选路径数都有硬上限，超限即返回 `truncated=true`，由调用方明确告知
//   「在搜索上限内未找到路径」不等于「整个数据库不存在路径」。
//
// 本文件被 include! 进 core lib.rs（crate root scope），不重复 use crate-root 导入。

/// 最大跳数硬上限（中间表数量）。
pub const ER_JOIN_PATH_MAX_HOPS: usize = 6;
/// 返回候选路径数硬上限。
pub const ER_JOIN_PATH_MAX_CANDIDATES: usize = 20;
/// 节点扩展次数硬上限（防止组合爆炸）。
pub const ER_JOIN_PATH_MAX_EXPANSIONS: usize = 4096;

/// 图中的一条可用关系边：两端表身份 + 该关系的 JOIN 计划。
#[derive(Clone, Debug, PartialEq)]
pub struct ErPathEdge {
    pub from: ErTableRef,
    pub to: ErTableRef,
    pub plan: ErJoinPlan,
}

/// 路径中的一步：沿某条关系从 `from` 走到 `to`。
#[derive(Clone, Debug, PartialEq)]
pub struct ErPathStep {
    pub from: ErTableRef,
    pub to: ErTableRef,
    /// 反向使用该关系（`reversed=true` 时，关系定义的 left/right 在这一步里互换）。
    pub reversed: bool,
    pub plan: ErJoinPlan,
}

/// 一条连接路径：有序步骤，完整保留每步的字段配对与必要过滤条件。
#[derive(Clone, Debug, PartialEq)]
pub struct ErJoinPath {
    pub steps: Vec<ErPathStep>,
}

impl ErJoinPath {
    pub fn hops(&self) -> usize {
        self.steps.len()
    }

    /// 路径涉及的全部表（起点在前，其余按步骤顺序）。
    pub fn tables(&self) -> Vec<ErTableRef> {
        self.steps
            .iter()
            .map(|step| step.from.clone())
            .chain(self.steps.last().map(|step| step.to.clone()))
            .collect()
    }
}

/// 搜索上限；默认值即硬上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErPathLimits {
    pub max_hops: usize,
    pub max_candidates: usize,
    pub max_expansions: usize,
}

impl Default for ErPathLimits {
    fn default() -> Self {
        Self {
            max_hops: ER_JOIN_PATH_MAX_HOPS,
            max_candidates: ER_JOIN_PATH_MAX_CANDIDATES,
            max_expansions: ER_JOIN_PATH_MAX_EXPANSIONS,
        }
    }
}

impl ErPathLimits {
    /// 调用方只能收紧：任一维度超过硬上限时按硬上限取值。
    pub fn bounded(
        max_hops: usize,
        max_candidates: usize,
        max_expansions: usize,
    ) -> Self {
        Self {
            max_hops: max_hops.clamp(1, ER_JOIN_PATH_MAX_HOPS),
            max_candidates: max_candidates.clamp(1, ER_JOIN_PATH_MAX_CANDIDATES),
            max_expansions: max_expansions.clamp(1, ER_JOIN_PATH_MAX_EXPANSIONS),
        }
    }
}

/// 搜索结果与边界事实。
#[derive(Clone, Debug, PartialEq)]
pub struct ErPathSearchOutcome {
    pub paths: Vec<ErJoinPath>,
    /// 因候选数或扩展次数触顶而提前停止：边界事实，必须随结果一起返回。
    pub truncated: bool,
    pub expansions: usize,
    /// 因自关联（同表两端）被排除的关系数。
    pub self_relations_skipped: usize,
    /// 起点等于终点：无需任何 JOIN 的空路径。
    pub degenerate_same_table: bool,
}

/// 在 `edges` 组成的无向图上枚举 `from` → `to` 的有界路径。
///
/// 按跳数迭代加深（1、2、… ≤ `max_hops`），因此结果天然按跳数升序；同一跳数内按
/// 邻接点的确定性顺序产出，便于调用方稳定比较与截断。`via` 非空时只保留包含全部
/// 必经表的路径。
pub fn er_search_join_paths(
    edges: &[ErPathEdge],
    from: &ErTableRef,
    to: &ErTableRef,
    via: &[ErTableRef],
    limits: ErPathLimits,
) -> ErPathSearchOutcome {
    let usable: Vec<&ErPathEdge> = edges.iter().filter(|edge| edge.from != edge.to).collect();
    let mut outcome = ErPathSearchOutcome {
        paths: Vec::new(),
        truncated: false,
        expansions: 0,
        self_relations_skipped: edges.len() - usable.len(),
        degenerate_same_table: from == to,
    };
    if from == to {
        outcome.paths.push(ErJoinPath { steps: Vec::new() });
        return outcome;
    }
    if usable.is_empty() {
        return outcome;
    }
    let adjacency = build_adjacency(&usable);
    let mut search = PathSearch {
        edges: &usable,
        adjacency: &adjacency,
        limits,
        to: to.clone(),
        via: via.iter().cloned().collect::<BTreeSet<_>>(),
        expansions: 0,
        truncated: false,
        paths: Vec::new(),
    };
    let mut visited: BTreeSet<ErTableRef> = BTreeSet::new();
    visited.insert(from.clone());
    for depth in 1..=limits.max_hops {
        search.walk_exact_depth(from.clone(), &mut visited, depth);
        if search.truncated {
            break;
        }
        if search.paths.len() >= limits.max_candidates {
            break;
        }
    }
    outcome.truncated = search.truncated;
    outcome.expansions = search.expansions;
    outcome.paths = search.paths;
    outcome
}

/// 邻接表按 (邻接表名, 角色, 关系 ID) 排序：同一关系的两个方向各有一次扩展，
/// 同表多角色表现为并列边，不会被合并。
fn build_adjacency(edges: &[&ErPathEdge]) -> BTreeMap<ErTableRef, Vec<(ErTableRef, usize, bool)>> {
    let mut adjacency: BTreeMap<ErTableRef, Vec<(ErTableRef, usize, bool)>> = BTreeMap::new();
    for (index, edge) in edges.iter().enumerate() {
        adjacency
            .entry(edge.from.clone())
            .or_default()
            .push((edge.to.clone(), index, false));
        adjacency
            .entry(edge.to.clone())
            .or_default()
            .push((edge.from.clone(), index, true));
    }
    for neighbours in adjacency.values_mut() {
        neighbours.sort_by(|left, right| {
            let key = |entry: &(ErTableRef, usize, bool)| {
                (
                    entry.0.display(),
                    edges[entry.1].plan.role.clone(),
                    edges[entry.1].plan.relationship_id.clone(),
                )
            };
            key(left).cmp(&key(right))
        });
    }
    adjacency
}

struct PathSearch<'a> {
    edges: &'a [&'a ErPathEdge],
    adjacency: &'a BTreeMap<ErTableRef, Vec<(ErTableRef, usize, bool)>>,
    limits: ErPathLimits,
    to: ErTableRef,
    via: BTreeSet<ErTableRef>,
    expansions: usize,
    truncated: bool,
    paths: Vec<ErJoinPath>,
}

impl PathSearch<'_> {
    /// 枚举恰好 `depth` 步的路径：到达终点即返回，不再向更深处延伸（迭代加深的分层由
    /// 调用方逐层推进），因此同层结果互不重复。
    fn walk_exact_depth(
        &mut self,
        node: ErTableRef,
        visited: &mut BTreeSet<ErTableRef>,
        depth: usize,
    ) {
        if self.truncated {
            return;
        }
        if self.expansions >= self.limits.max_expansions
            || self.paths.len() >= self.limits.max_candidates
        {
            self.truncated = true;
            return;
        }
        let Some(neighbours) = self.adjacency.get(&node).cloned() else {
            return;
        };
        let mut steps: Vec<ErPathStep> = Vec::with_capacity(depth);
        self.walk(node, &neighbours, visited, depth, &mut steps);
    }

    fn walk(
        &mut self,
        node: ErTableRef,
        neighbours: &[(ErTableRef, usize, bool)],
        visited: &mut BTreeSet<ErTableRef>,
        remaining: usize,
        steps: &mut Vec<ErPathStep>,
    ) {
        if self.truncated {
            return;
        }
        if self.expansions >= self.limits.max_expansions
            || self.paths.len() >= self.limits.max_candidates
        {
            self.truncated = true;
            return;
        }
        self.expansions += 1;
        for (neighbour, index, reversed) in neighbours.iter().cloned() {
            if self.truncated {
                return;
            }
            if visited.contains(&neighbour) {
                continue;
            }
            steps.push(ErPathStep {
                from: node.clone(),
                to: neighbour.clone(),
                reversed,
                plan: self.edges[index].plan.clone(),
            });
            visited.insert(neighbour.clone());
            if neighbour == self.to {
                // 只记录「恰好走满本层步数」的路径：更早到达终点属于更短的路径，
                // 由上一轮迭代加深负责，不能在每层重复计入。
                if remaining == 1 && self.via.iter().all(|table| visited.contains(table)) {
                    self.paths.push(ErJoinPath {
                        steps: steps.clone(),
                    });
                }
            } else if remaining > 1
                && let Some(next) = self.adjacency.get(&neighbour).cloned()
            {
                self.walk(neighbour.clone(), &next, visited, remaining - 1, steps);
            }
            visited.remove(&neighbour);
            steps.pop();
        }
    }
}
#[cfg(test)]
mod er_relationship_path_tests {
    use super::*;

    fn table(name: &str) -> ErTableRef {
        ErTableRef {
            database: "sales".into(),
            schema: None,
            name: name.into(),
        }
    }

    fn edge(from: &str, to: &str, id: &str, role: &str) -> ErPathEdge {
        ErPathEdge {
            from: table(from),
            to: table(to),
            plan: ErJoinPlan {
                relationship_id: id.into(),
                rev: 1,
                role: role.into(),
                left_entity: er_entity_id(&table(from)),
                right_entity: er_entity_id(&table(to)),
                pairs: vec![ErColumnPair {
                    left_column: format!("{from}_id"),
                    right_column: format!("{to}_id"),
                }],
                filters: Vec::new(),
                warnings: Vec::new(),
            },
        }
    }

    fn limits() -> ErPathLimits {
        ErPathLimits::default()
    }

    fn names(path: &ErJoinPath) -> Vec<String> {
        path.tables().iter().map(|table| table.name.clone()).collect()
    }

    #[test]
    fn direct_path_is_found_first() {
        let edges = vec![edge("orders", "items", "r1", "items")];

        let outcome = er_search_join_paths(&edges, &table("orders"), &table("items"), &[], limits());

        assert_eq!(outcome.paths.len(), 1);
        assert_eq!(names(&outcome.paths[0]), vec!["orders", "items"]);
        assert!(!outcome.truncated);
    }

    #[test]
    fn multi_hop_path_uses_intermediate_table_and_keeps_filters() {
        let mut hop = edge("orders", "customers", "r1", "customer");
        hop.plan.filters = vec![ErRequiredFilter {
            side: ErRelationSide::Right,
            column_id: "sales::customers::is_deleted".into(),
            op: ErFilterOp::Eq,
            literal: ErLiteral::Int(0),
        }];
        let edges = vec![hop, edge("customers", "region", "r2", "region")];

        let outcome = er_search_join_paths(&edges, &table("orders"), &table("region"), &[], limits());

        assert_eq!(outcome.paths.len(), 1);
        let path = &outcome.paths[0];
        assert_eq!(names(path), vec!["orders", "customers", "region"]);
        assert_eq!(
            path.steps[0].plan.filters.len(),
            1,
            "必要过滤条件不得在路径里丢失"
        );
        // 从 customers 反向走回 orders 时左右端互换，投影据此翻转配对。
        assert!(!path.steps[1].reversed);
        let reversed = er_search_join_paths(&edges, &table("region"), &table("orders"), &[], limits());
        assert!(reversed.paths[0].steps[1].reversed);
    }

    #[test]
    fn ambiguous_graph_returns_bounded_candidates() {
        let edges = vec![
            edge("a", "m1", "r1", "via_m1"),
            edge("m1", "b", "r2", "to_b"),
            edge("a", "m2", "r3", "via_m2"),
            edge("m2", "b", "r4", "to_b"),
        ];

        let outcome = er_search_join_paths(&edges, &table("a"), &table("b"), &[], limits());

        assert_eq!(outcome.paths.len(), 2, "两条同长度路径都必须返回，不能只给最短");
        assert!(outcome.paths.iter().all(|path| path.hops() == 2));
    }

    #[test]
    fn via_constraint_keeps_only_paths_through_required_table() {
        let edges = vec![
            edge("a", "m1", "r1", "via_m1"),
            edge("m1", "b", "r2", "to_b"),
            edge("a", "m2", "r3", "via_m2"),
            edge("m2", "b", "r4", "to_b"),
        ];

        let outcome =
            er_search_join_paths(&edges, &table("a"), &table("b"), &[table("m2")], limits());

        assert_eq!(outcome.paths.len(), 1);
        assert!(names(&outcome.paths[0]).contains(&"m2".to_string()));
    }

    #[test]
    fn same_table_multiple_roles_stay_separate_paths() {
        let edges = vec![edge("a", "b", "r1", "buyer"), edge("a", "b", "r2", "reviewer")];

        let outcome = er_search_join_paths(&edges, &table("a"), &table("b"), &[], limits());

        assert_eq!(
            outcome.paths.len(),
            2,
            "同表多角色应表现为两条并列候选，而不是被合并"
        );
        let roles: BTreeSet<&str> = outcome
            .paths
            .iter()
            .flat_map(|path| path.steps.iter().map(|step| step.plan.role.as_str()))
            .collect();
        assert_eq!(roles, BTreeSet::from(["buyer", "reviewer"]));
    }

    #[test]
    fn cycles_do_not_loop() {
        // b 与 c 之间有两条关系（其中一条是回边）；路径不得在同一对表之间反复往返。
        let edges = vec![
            edge("a", "b", "r1", "out"),
            edge("b", "c", "r2", "out"),
            edge("c", "b", "r3", "back"),
        ];

        let outcome = er_search_join_paths(&edges, &table("a"), &table("c"), &[], limits());

        assert_eq!(outcome.paths.len(), 2, "两条 b↔c 关系各自构成一条候选路径");
        for path in &outcome.paths {
            let visited = names(path);
            let mut unique = visited.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), visited.len(), "路径不得重复经过同一张表");
            assert_eq!(visited, vec!["a", "b", "c"]);
        }
    }

    #[test]
    fn self_relations_are_skipped_and_reported() {
        let edges = vec![edge("a", "a", "self", "manager"), edge("a", "b", "r1", "child")];

        let outcome = er_search_join_paths(&edges, &table("a"), &table("b"), &[], limits());

        assert_eq!(outcome.self_relations_skipped, 1);
        assert_eq!(outcome.paths.len(), 1);
    }

    #[test]
    fn search_limits_truncate_and_report_boundary() {
        let mut edges = Vec::new();
        for index in 0..6 {
            edges.push(edge("start", &format!("m{index}"), &format!("r{index}a"), "out"));
            edges.push(edge(&format!("m{index}"), "end", &format!("r{index}b"), "in"));
        }

        let outcome = er_search_join_paths(
            &edges,
            &table("start"),
            &table("end"),
            &[],
            ErPathLimits::bounded(6, 2, ER_JOIN_PATH_MAX_EXPANSIONS),
        );

        assert_eq!(outcome.paths.len(), 2, "候选数上限必须生效");
        assert!(outcome.truncated, "触顶必须作为边界事实返回");
        assert!(
            ErPathLimits::bounded(99, 99, 99).max_hops <= ER_JOIN_PATH_MAX_HOPS,
            "调用方只能收紧上限，不能放宽"
        );
    }

    #[test]
    fn no_path_within_limits_is_reported_as_empty_not_as_proof() {
        let edges = vec![edge("a", "b", "r1", "child")];

        let outcome = er_search_join_paths(&edges, &table("a"), &table("z"), &[], limits());

        assert!(outcome.paths.is_empty());
        assert!(!outcome.truncated, "触顶与确实无路径必须可区分");
    }

    #[test]
    fn same_start_and_end_yields_degenerate_path() {
        let outcome = er_search_join_paths(&[], &table("a"), &table("a"), &[], limits());

        assert!(outcome.degenerate_same_table);
        assert_eq!(outcome.paths.len(), 1);
        assert_eq!(outcome.paths[0].steps.len(), 0);
    }
}
