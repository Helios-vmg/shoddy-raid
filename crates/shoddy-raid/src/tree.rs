#![allow(dead_code)]

use std::path::PathBuf;
use anyhow::{Result, anyhow};

/// Represents a directory entry in the tree structure
#[derive(Debug, Clone)]
pub enum DirEntry {
    /// A directory containing other entries
    Dir(DirNode),
    /// A file to be added to the pool
    File(FileInfo),
}

/// Represents a directory node with a name and its children
#[derive(Debug, Clone)]
pub struct DirNode {
    pub name: String,
    pub children: Vec<DirEntry>,
}

/// Represents file information for files to be added
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
}

impl DirNode {
    /// Creates a new directory node with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            children: Vec::new(),
        }
    }

    /// Adds a child entry to this directory
    pub fn add_child(&mut self, entry: DirEntry) {
        self.children.push(entry);
    }

    /// Returns the total number of files in this directory tree
    pub fn file_count(&self) -> usize {
        self.children.iter().map(|child| match child {
            DirEntry::Dir(node) => node.file_count(),
            DirEntry::File(_) => 1,
        }).sum()
    }

    /// Returns the total size of all files in this directory tree
    pub fn total_size(&self) -> u64 {
        self.children.iter().map(|child| match child {
            DirEntry::Dir(node) => node.total_size(),
            DirEntry::File(file) => file.size,
        }).sum()
    }
}

impl FileInfo {
    /// Creates a new FileInfo from a path
    pub fn from_path(path: PathBuf) -> Result<Self> {
        let metadata = std::fs::metadata(&path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        
        Ok(Self {
            name,
            path,
            size: metadata.len(),
        })
    }
}

/// Recursively scans a directory and builds a DirEntry tree
pub fn scan_directory(path: PathBuf) -> Result<DirEntry> {
    let metadata = std::fs::metadata(&path)?;
    
    if metadata.is_file() {
        FileInfo::from_path(path).map(DirEntry::File)
    } else if metadata.is_dir() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string());
        
        let mut children = Vec::new();
        
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let child_path = entry.path();
            let child_entry = scan_directory(child_path)?;
            children.push(child_entry);
        }
        
        Ok(DirEntry::Dir(DirNode {
            name,
            children,
        }))
    } else {
        Err(anyhow!("Path is neither a file nor a directory"))
    }
}
