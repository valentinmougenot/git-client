//! Builds a directory tree out of a flat list of slash-separated paths.
//!
//! Both the File List (paths like `src/ui/mod.rs`) and the Branches list (names
//! like `feature/login`) are flat lists that read better as a hierarchy. This
//! groups them into directory nodes and leaves, generic over the leaf type via a
//! `path_of` accessor. Directory chains with a single child are compressed into
//! one node (`src/ui` rather than `src` ▸ `ui`), the way most editors show them.

/// A node in the tree: either a directory holding more nodes, or a leaf `T`
/// (a file entry, a branch, …).
pub enum Node<'a, T> {
    Dir {
        /// The segment to display, e.g. `ui` — or `ui/widget` when a
        /// single-child chain was compressed.
        name: String,
        /// The full path from the root to this directory, used as the collapse
        /// key.
        path: String,
        children: Vec<Node<'a, T>>,
    },
    Leaf(&'a T),
}

/// Group a flat list of entries into a tree, splitting each entry's path (from
/// `path_of`) on `/`. Directories come before leaves at each level; within each
/// kind the input order is preserved.
pub fn build<'a, T>(entries: &'a [T], path_of: impl Fn(&'a T) -> &'a str) -> Vec<Node<'a, T>> {
    let items: Vec<(&'a T, Vec<&'a str>)> = entries
        .iter()
        .map(|entry| (entry, path_of(entry).split('/').collect()))
        .collect();
    build_level(&items, "")
}

/// The paths of every leaf under a list of nodes, depth-first. Used for a
/// directory's "select all" checkbox and its leaf count (`.len()`).
pub fn leaf_paths<'a, T>(nodes: &[Node<'a, T>], path_of: impl Fn(&'a T) -> &'a str) -> Vec<String> {
    let mut out = Vec::new();
    collect_paths(nodes, &path_of, &mut out);
    out
}

fn collect_paths<'a, T>(
    nodes: &[Node<'a, T>],
    path_of: &impl Fn(&'a T) -> &'a str,
    out: &mut Vec<String>,
) {
    for node in nodes {
        match node {
            Node::Leaf(entry) => out.push(path_of(entry).to_string()),
            Node::Dir { children, .. } => collect_paths(children, path_of, out),
        }
    }
}

fn build_level<'a, T>(items: &[(&'a T, Vec<&'a str>)], prefix: &str) -> Vec<Node<'a, T>> {
    // Directory buckets, kept in first-seen order; leaves at this level held
    // aside so they render after the directories.
    let mut dirs: Vec<(String, Vec<(&'a T, Vec<&'a str>)>)> = Vec::new();
    let mut leaves: Vec<Node<'a, T>> = Vec::new();

    for (entry, segments) in items {
        if segments.len() <= 1 {
            leaves.push(Node::Leaf(entry));
            continue;
        }
        let head = segments[0];
        let rest = segments[1..].to_vec();
        match dirs.iter_mut().find(|(name, _)| name == head) {
            Some((_, group)) => group.push((entry, rest)),
            None => dirs.push((head.to_string(), vec![(*entry, rest)])),
        }
    }

    let mut out = Vec::with_capacity(dirs.len() + leaves.len());
    for (name, group) in dirs {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let children = build_level(&group, &path);
        out.push(compress(name, path, children));
    }
    out.extend(leaves);
    out
}

/// Collapse a directory that holds exactly one subdirectory (and no leaves) into
/// a single node, recursively, so long single-child chains read as one row.
fn compress<'a, T>(name: String, path: String, mut children: Vec<Node<'a, T>>) -> Node<'a, T> {
    if children.len() == 1 && matches!(children[0], Node::Dir { .. }) {
        if let Node::Dir {
            name: child_name,
            path: child_path,
            children: grandchildren,
        } = children.pop().expect("len checked above")
        {
            return compress(format!("{name}/{child_name}"), child_path, grandchildren);
        }
    }
    Node::Dir {
        name,
        path,
        children,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{ChangeKind, FileEntry};

    fn entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            change: ChangeKind::Modified,
        }
    }

    fn path_of(entry: &FileEntry) -> &str {
        entry.path.as_str()
    }

    /// A flat description of the tree for assertions: one `"depth name"` line per
    /// node, depth-first, directories before files.
    fn flatten(nodes: &[Node<'_, FileEntry>], depth: usize, out: &mut Vec<String>) {
        for node in nodes {
            match node {
                Node::Dir { name, children, .. } => {
                    out.push(format!("{depth} {name}/"));
                    flatten(children, depth + 1, out);
                }
                Node::Leaf(e) => out.push(format!("{depth} {}", e.path.rsplit('/').next().unwrap())),
            }
        }
    }

    fn render(entries: &[FileEntry]) -> Vec<String> {
        let mut out = Vec::new();
        flatten(&build(entries, path_of), 0, &mut out);
        out
    }

    #[test]
    fn groups_files_under_their_directories() {
        let entries = [entry("src/ui/mod.rs"), entry("src/app.rs"), entry("README.md")];
        assert_eq!(
            render(&entries),
            vec![
                "0 src/".to_string(),
                "1 ui/".to_string(),
                "2 mod.rs".to_string(),
                "1 app.rs".to_string(),
                "0 README.md".to_string(),
            ]
        );
    }

    #[test]
    fn compresses_single_child_chains() {
        let entries = [entry("a/b/c/deep.rs")];
        // The whole chain collapses to one directory node.
        assert_eq!(
            render(&entries),
            vec!["0 a/b/c/".to_string(), "1 deep.rs".to_string()]
        );
    }

    #[test]
    fn does_not_compress_when_a_directory_branches() {
        let entries = [entry("src/ui/a.rs"), entry("src/git/b.rs")];
        assert_eq!(
            render(&entries),
            vec![
                "0 src/".to_string(),
                "1 ui/".to_string(),
                "2 a.rs".to_string(),
                "1 git/".to_string(),
                "2 b.rs".to_string(),
            ]
        );
    }

    #[test]
    fn directory_path_is_the_full_prefix() {
        let entries = [entry("src/ui/mod.rs")];
        let nodes = build(&entries, path_of);
        // Compressed to `src/ui`, with the full path as its collapse key.
        match &nodes[0] {
            Node::Dir { name, path, .. } => {
                assert_eq!(name, "src/ui");
                assert_eq!(path, "src/ui");
            }
            _ => panic!("expected a directory"),
        }
    }

    #[test]
    fn file_paths_collects_every_leaf() {
        let entries = [entry("src/a.rs"), entry("src/sub/b.rs"), entry("top.rs")];
        // Depth-first, mirroring the rendered order: subdirectories before
        // files at each level.
        assert_eq!(
            leaf_paths(&build(&entries, path_of), path_of),
            vec![
                "src/sub/b.rs".to_string(),
                "src/a.rs".to_string(),
                "top.rs".to_string(),
            ]
        );
    }
}
