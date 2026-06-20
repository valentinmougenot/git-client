//! Lane layout for the commit graph in the History view.
//!
//! Given the commits newest-first (each with its parent SHAs), this assigns
//! every commit to a vertical lane and produces, per row, the line segments to
//! draw: lines entering from the top, the node, and lines leaving to the bottom.
//! The UI renders each row's segments in its own cell, so a straight lane is a
//! vertical line shared by stacked rows, a branch is a diagonal leaving a node,
//! and a merge is a diagonal arriving at one.
//!
//! The algorithm is the usual "swimlane" walk: a list of lanes, each holding the
//! SHA it is waiting to render next. Processing a commit fills the lane(s)
//! waiting for it, then hands those lanes on to its parents.

/// A commit reduced to what the layout needs.
pub struct Commit<'a> {
    pub sha: &'a str,
    pub parents: &'a [String],
}

/// One line segment within a row, between two lanes. Coordinates are lane
/// indices; the renderer maps them to x positions and the segment spans either
/// the top half (top edge → node row) or bottom half (node row → bottom edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    /// Palette index, so a lane keeps a stable color while it lasts.
    pub color: usize,
}

/// The graph rendering data for a single commit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The lane the commit's node sits in.
    pub node_lane: usize,
    pub node_color: usize,
    /// How many lanes are occupied around this row (max of incoming/outgoing),
    /// used to size the row's graph cell.
    pub lanes: usize,
    /// Segments from the top edge down to the node row.
    pub top: Vec<Edge>,
    /// Segments from the node row down to the bottom edge.
    pub bottom: Vec<Edge>,
}

/// Lay out the commit graph. `commits` must be newest-first with parents listed
/// after their children (topological order), as `load_history` provides.
pub fn layout(commits: &[Commit<'_>]) -> Vec<Row> {
    // `lanes[i]` is the SHA lane `i` is currently waiting to render, or `None`
    // when the lane is free.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        let lanes_in = lanes.clone();

        // Every lane waiting for this commit converges on it; the leftmost
        // becomes the node's lane, the rest are freed.
        let converging: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, sha)| sha.as_deref() == Some(commit.sha))
            .map(|(i, _)| i)
            .collect();

        let node_lane = match converging.first() {
            Some(&lane) => lane,
            None => free_lane(&mut lanes),
        };
        for &lane in converging.iter().skip(1) {
            lanes[lane] = None;
        }

        // The first parent continues the node's lane; extra parents (a merge)
        // reuse a lane already heading to them, or open a fresh one.
        let mut parents = commit.parents.iter();
        lanes[node_lane] = parents.next().cloned();
        let mut merge_lanes: Vec<usize> = Vec::new();
        for parent in parents {
            let lane = match lanes.iter().position(|s| s.as_deref() == Some(parent.as_str())) {
                Some(lane) => lane,
                None => {
                    let lane = free_lane(&mut lanes);
                    lanes[lane] = Some(parent.clone());
                    lane
                }
            };
            merge_lanes.push(lane);
        }

        let lanes_out = lanes.clone();

        // Incoming lines: each active lane drops to the node row, converging
        // lanes angling into the node and others holding their column.
        let mut top = Vec::new();
        for (lane, sha) in lanes_in.iter().enumerate() {
            if sha.is_none() {
                continue;
            }
            let to = if converging.contains(&lane) {
                node_lane
            } else {
                lane
            };
            top.push(Edge {
                from: lane,
                to,
                color: color_of(lane),
            });
        }

        // Outgoing lines: the node's lane and merge lanes leave from the node,
        // pass-through lanes hold their column.
        let mut bottom = Vec::new();
        for (lane, sha) in lanes_out.iter().enumerate() {
            if sha.is_none() {
                continue;
            }
            if lane == node_lane || merge_lanes.contains(&lane) {
                bottom.push(Edge {
                    from: node_lane,
                    to: lane,
                    color: color_of(lane),
                });
            } else {
                bottom.push(Edge {
                    from: lane,
                    to: lane,
                    color: color_of(lane),
                });
            }
        }

        rows.push(Row {
            node_lane,
            node_color: color_of(node_lane),
            lanes: lanes_in.len().max(lanes_out.len()),
            top,
            bottom,
        });
    }

    rows
}

/// The first free lane, growing the lane list if all are occupied.
fn free_lane(lanes: &mut Vec<Option<String>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(lane) => lane,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

/// Number of distinct lane colors the renderer cycles through.
pub const COLORS: usize = 6;

fn color_of(lane: usize) -> usize {
    lane % COLORS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit<'a>(sha: &'a str, parents: &'a [String]) -> Commit<'a> {
        Commit { sha, parents }
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let p = |s: &str| vec![s.to_string()];
        let (a, b, c) = (p("b"), p("c"), Vec::new());
        let commits = [commit("a", &a), commit("b", &b), commit("c", &c)];
        let rows = layout(&commits);

        assert!(rows.iter().all(|r| r.node_lane == 0));
        assert!(rows.iter().all(|r| r.lanes == 1));
        // The tip has no incoming line; the root has no outgoing line.
        assert!(rows[0].top.is_empty());
        assert!(rows[2].bottom.is_empty());
        // The middle commit is a straight vertical: in at lane 0, out at lane 0.
        assert_eq!(rows[1].top, vec![Edge { from: 0, to: 0, color: 0 }]);
        assert_eq!(rows[1].bottom, vec![Edge { from: 0, to: 0, color: 0 }]);
    }

    #[test]
    fn branch_opens_a_second_lane() {
        // a and b are two tips; both descend from c.
        //   a (parent c), b (parent c), c (root)
        let ac = vec!["c".to_string()];
        let bc = vec!["c".to_string()];
        let croot: Vec<String> = Vec::new();
        let commits = [commit("a", &ac), commit("b", &bc), commit("c", &croot)];
        let rows = layout(&commits);

        assert_eq!(rows[0].node_lane, 0);
        assert_eq!(rows[1].node_lane, 1); // b takes a fresh lane
        // c is where both lanes converge back to lane 0.
        assert_eq!(rows[2].node_lane, 0);
        assert!(rows[2].top.iter().any(|e| e.from == 1 && e.to == 0));
        assert!(rows[2].bottom.is_empty());
    }

    #[test]
    fn merge_commit_has_two_parents_leaving_the_node() {
        // m merges l and r; both are roots.
        //   m (parents l, r), l (root), r (root)
        let lr = vec!["l".to_string(), "r".to_string()];
        let lroot: Vec<String> = Vec::new();
        let rroot: Vec<String> = Vec::new();
        let commits = [commit("m", &lr), commit("l", &lroot), commit("r", &rroot)];
        let rows = layout(&commits);

        assert_eq!(rows[0].node_lane, 0);
        // Two lines leave the merge node: to lane 0 (first parent) and lane 1.
        assert!(rows[0].bottom.iter().any(|e| e.from == 0 && e.to == 0));
        assert!(rows[0].bottom.iter().any(|e| e.from == 0 && e.to == 1));
        assert_eq!(rows[0].lanes, 2);
    }

    #[test]
    fn lane_count_never_underflows_width() {
        let ac = vec!["c".to_string()];
        let bc = vec!["c".to_string()];
        let croot: Vec<String> = Vec::new();
        let commits = [commit("a", &ac), commit("b", &bc), commit("c", &croot)];
        let rows = layout(&commits);
        // While both branches are live, the layout is two lanes wide.
        assert_eq!(rows[1].lanes, 2);
    }
}
