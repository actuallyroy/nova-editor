// Workspace = open documents + file tree.
// File tree is a flat list of FileNode with depth, rebuilt when a folder toggles.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use glyphon::FontSystem;

use crate::document::Document;

pub struct FileNode {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
}

pub struct FileTree {
    pub root: PathBuf,
    pub nodes: Vec<FileNode>,
    expanded_set: HashSet<PathBuf>,
}

impl FileTree {
    pub fn new(root: PathBuf) -> Self {
        let mut t = Self {
            root,
            nodes: Vec::new(),
            expanded_set: HashSet::new(),
        };
        t.rebuild();
        t
    }

    pub fn rebuild(&mut self) {
        self.nodes.clear();
        self.add_children(&self.root.clone(), 0);
    }

    fn add_children(&mut self, dir: &Path, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut children: Vec<(PathBuf, String, bool)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            children.push((path, name, is_dir));
        }
        children.sort_by(|a, b| match (a.2, b.2) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.1.to_lowercase().cmp(&b.1.to_lowercase()),
        });
        for (path, name, is_dir) in children {
            let expanded = is_dir && self.expanded_set.contains(&path);
            self.nodes.push(FileNode {
                path: path.clone(),
                name,
                is_dir,
                depth,
                expanded,
            });
            if expanded {
                self.add_children(&path, depth + 1);
            }
        }
    }

    /// Re-read the tree from disk (preserving which folders are expanded).
    pub fn refresh(&mut self) {
        self.rebuild();
    }

    /// Collapse every folder.
    pub fn collapse_all(&mut self) {
        self.expanded_set.clear();
        self.rebuild();
    }

    /// Force a folder open (used when creating an item inside it).
    pub fn expand(&mut self, path: &Path) {
        if self.expanded_set.insert(path.to_path_buf()) {
            self.rebuild();
        }
    }

    pub fn toggle(&mut self, idx: usize) {
        let Some(node) = self.nodes.get(idx) else {
            return;
        };
        if !node.is_dir {
            return;
        }
        let path = node.path.clone();
        if self.expanded_set.contains(&path) {
            self.expanded_set.remove(&path);
        } else {
            self.expanded_set.insert(path);
        }
        self.rebuild();
    }
}

pub struct Workspace {
    pub documents: Vec<Document>,
    pub active: Option<usize>,
    pub tree: FileTree,
}

impl Workspace {
    pub fn new(root: PathBuf) -> Self {
        Self {
            documents: Vec::new(),
            active: None,
            tree: FileTree::new(root),
        }
    }

    pub fn active_doc(&self) -> Option<&Document> {
        self.active.and_then(|i| self.documents.get(i))
    }

    pub fn active_doc_mut(&mut self) -> Option<&mut Document> {
        let i = self.active?;
        self.documents.get_mut(i)
    }

    /// Create a new file or folder inside `parent`, then refresh the tree
    /// (keeping `parent` expanded so the new item is visible). Errors if the
    /// name already exists.
    pub fn create_entry(&mut self, parent: &Path, name: &str, is_dir: bool) -> std::io::Result<PathBuf> {
        let path = parent.join(name);
        if is_dir {
            std::fs::create_dir(&path)?;
        } else {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
        }
        self.tree.expand(parent);
        self.tree.rebuild();
        Ok(path)
    }

    pub fn open_file(&mut self, path: &Path, fs: &mut FontSystem) -> std::io::Result<()> {
        // If already open, switch to it.
        for (i, d) in self.documents.iter().enumerate() {
            if d.path.as_deref() == Some(path) {
                self.active = Some(i);
                return Ok(());
            }
        }
        let contents = std::fs::read_to_string(path)?;
        let doc = Document::new(Some(path.to_path_buf()), contents, fs);
        self.documents.push(doc);
        self.active = Some(self.documents.len() - 1);
        Ok(())
    }

    pub fn close_active(&mut self) {
        let Some(i) = self.active else {
            return;
        };
        if i >= self.documents.len() {
            self.active = None;
            return;
        }
        self.documents.remove(i);
        if self.documents.is_empty() {
            self.active = None;
        } else if i >= self.documents.len() {
            self.active = Some(self.documents.len() - 1);
        } else {
            self.active = Some(i);
        }
    }

    pub fn close_idx(&mut self, idx: usize) {
        if idx >= self.documents.len() {
            return;
        }
        self.documents.remove(idx);
        match self.active {
            Some(a) if a == idx => {
                if self.documents.is_empty() {
                    self.active = None;
                } else if idx >= self.documents.len() {
                    self.active = Some(self.documents.len() - 1);
                } else {
                    self.active = Some(idx);
                }
            }
            Some(a) if a > idx => self.active = Some(a - 1),
            _ => {}
        }
    }

    pub fn switch_to(&mut self, idx: usize) {
        if idx < self.documents.len() {
            self.active = Some(idx);
        }
    }
}
