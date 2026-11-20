use anyhow::{Context as _, Result};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub struct Folder {
    pub path: PathBuf,
    pub depth: usize,
    pub expanded: bool,
}
pub struct FolderTree {
    pub rows: Vec<Folder>,
    pub selected: usize,
}
impl FolderTree {
    pub fn new(root: PathBuf) -> Result<Self> {
        let mut tree = Self {
            rows: vec![Folder {
                path: root,
                depth: 0,
                expanded: false,
            }],
            selected: 0,
        };
        tree.expand(0)?;
        Ok(tree)
    }
    pub fn path(&self) -> &Path {
        &self.rows[self.selected].path
    }
    pub fn step(&mut self, offset: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(offset)
            .min(self.rows.len() - 1);
    }
    fn expand(&mut self, index: usize) -> Result<()> {
        let folder = &self.rows[index];
        let children = fs::read_dir(&folder.path)
            .with_context(|| format!("폴더를 읽을 수 없습니다 · {}", folder.path.display()))?;
        let mut paths = Vec::new();
        for child in children {
            let child = child?;
            match child.file_type() {
                Ok(kind) if kind.is_dir() => paths.push(child.path()),
                Ok(_) => (),
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        paths.sort();
        let depth = folder.depth + 1;
        self.rows[index].expanded = true;
        self.rows.splice(
            index + 1..index + 1,
            paths.into_iter().map(|path| Folder {
                path,
                depth,
                expanded: false,
            }),
        );
        Ok(())
    }
    pub fn right(&mut self) -> Result<()> {
        if !self.rows[self.selected].expanded {
            self.expand(self.selected)?;
        } else if self
            .rows
            .get(self.selected + 1)
            .is_some_and(|item| item.depth > self.rows[self.selected].depth)
        {
            self.step(1);
        }
        Ok(())
    }
    pub fn left(&mut self) -> Result<()> {
        let folder = &self.rows[self.selected];
        if folder.expanded {
            let start = self.selected + 1;
            let count = self.rows[start..]
                .iter()
                .take_while(|item| item.depth > folder.depth)
                .count();
            self.rows.drain(start..start + count);
            self.rows[self.selected].expanded = false;
            Ok(())
        } else {
            self.parent()
        }
    }
    pub fn parent(&mut self) -> Result<()> {
        let Some(parent) = self.path().parent() else {
            return Ok(());
        };
        if let Some(index) = self.rows.iter().position(|f| f.path == parent) {
            self.selected = index;
        } else {
            *self = Self::new(parent.to_owned())?;
        }
        Ok(())
    }
    pub fn refresh(&mut self) -> Result<()> {
        let mut next = Self::new(self.rows[0].path.clone())?;
        let expanded: std::collections::BTreeSet<_> = self
            .rows
            .iter()
            .filter(|x| x.expanded)
            .map(|x| &x.path)
            .collect();
        if !self.rows[0].expanded {
            next.left()?;
        }
        let mut index = 1;
        while index < next.rows.len() {
            if expanded.contains(&next.rows[index].path) {
                next.expand(index)?;
            }
            index += 1;
        }
        next.selected = self
            .path()
            .ancestors()
            .find_map(|path| next.rows.iter().position(|f| f.path == path))
            .unwrap_or(0);
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lazy_navigation_reaches_children_siblings_and_parent() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("project");
        let nested = root.join(".hidden/inner");
        fs::create_dir_all(&nested)?;
        fs::write(root.join("file"), "")?;
        std::os::unix::fs::symlink(&root, root.join("loop"))?;
        let mut tree = FolderTree::new(root.clone())?;
        assert_eq!(tree.rows.len(), 2);
        tree.step(1);
        tree.right()?;
        tree.right()?;
        assert_eq!(tree.path(), nested);
        fs::remove_dir(&nested)?;
        tree.refresh()?;
        assert_eq!(tree.path(), root.join(".hidden"));
        tree.left()?;
        tree.parent()?;
        tree.parent()?;
        assert_eq!(tree.path(), tmp.path());
        Ok(())
    }
}
