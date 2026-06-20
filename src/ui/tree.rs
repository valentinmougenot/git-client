//! Builds a directory tree out of the flat File List for the Diff sidebar.
//!
//! Git reports changes as a flat list of slash-separated paths. This groups them
//! into a hierarchy of directory nodes and file leaves so the UI can render a
//! collapsible tree. Directory chains with a single child are compressed into
//! one node (`src/ui` rather than `src` ▸ `ui`), the way most editors show them.

use crate::git::FileEntry;

/// A node in the File Tree: either a directory holding more nodes, or a file.
pub enum Node<'a> {
    Dir {
        /// The segment to display, e.g. `ui` — or `ui/widget` when a
        /// single-child chain was compressed.
        name: String,
        /// The full path from the repo root to this directory, used as the
        /// collapse key.
        path: String,
        children: Vec<Node<'a>>,
    },
    File(&'a FileEntry),
}

/// Group a flat list of entries into a directory tree. Directories come before
/// files at each level; within each kind the input order is preserved.
pub fn build(entries: &[FileEntry]) -> Vec<Node<'_>> {
    let items: Vec<(&FileEntry, Vec<&str>)> = entries
        .iter()
        .map(|entry| (entry, entry.path.split('/').collect()))
        .collect();
    build_level(&items, "")
}

/// The paths of every file leaf under a list of nodes, depth-first. Used for a
/// directory's "select all" checkbox and its file count (`.len()`).
pub fn file_paths(nodes: &[Node<'_>]) -> Vec<String> {
    let mut out = Vec::new();
    collect_paths(nodes, &mut out);
    out
}

fn collect_paths(nodes: &[Node<'_>], out: &mut Vec<String>) {
    for node in nodes {
        match node {
            Node::File(entry) => out.push(entry.path.clone()),
            Node::Dir { children, .. } => collect_paths(children, out),
        }
    }
}

fn build_level<'a>(items: &[(&'a FileEntry, Vec<&'a str>)], prefix: &str) -> Vec<Node<'a>> {
    // Directory buckets, kept in first-seen order; files at this level held aside
    // so they render after the directories.
    let mut dirs: Vec<(String, Vec<(&'a FileEntry, Vec<&'a str>)>)> = Vec::new();
    let mut files: Vec<Node<'a>> = Vec::new();

    for (entry, segments) in items {
        if segments.len() <= 1 {
            files.push(Node::File(entry));
            continue;
        }
        let head = segments[0];
        let rest = segments[1..].to_vec();
        match dirs.iter_mut().find(|(name, _)| name == head) {
            Some((_, group)) => group.push((entry, rest)),
            None => dirs.push((head.to_string(), vec![(*entry, rest)])),
        }
    }

    let mut out = Vec::with_capacity(dirs.len() + files.len());
    for (name, group) in dirs {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let children = build_level(&group, &path);
        out.push(compress(name, path, children));
    }
    out.extend(files);
    out
}

/// Collapse a directory that holds exactly one subdirectory (and no files) into
/// a single node, recursively, so long single-child chains read as one row.
fn compress<'a>(name: String, path: String, mut children: Vec<Node<'a>>) -> Node<'a> {
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
    use crate::git::ChangeKind;

    fn entry(path: &str) -> FileEntry {
        FileEntry {
            path: path.to_string(),
            change: ChangeKind::Modified,
        }
    }

    /// A flat description of the tree for assertions: one `"depth name"` line per
    /// node, depth-first, directories before files.
    fn flatten(nodes: &[Node<'_>], depth: usize, out: &mut Vec<String>) {
        for node in nodes {
            match node {
                Node::Dir { name, children, .. } => {
                    out.push(format!("{depth} {name}/"));
                    flatten(children, depth + 1, out);
                }
                Node::File(e) => out.push(format!("{depth} {}", e.path.rsplit('/').next().unwrap())),
            }
        }
    }

    fn render(entries: &[FileEntry]) -> Vec<String> {
        let mut out = Vec::new();
        flatten(&build(entries), 0, &mut out);
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
        let nodes = build(&entries);
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
            file_paths(&build(&entries)),
            vec![
                "src/sub/b.rs".to_string(),
                "src/a.rs".to_string(),
                "top.rs".to_string(),
            ]
        );
    }
}
