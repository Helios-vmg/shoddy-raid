use std::path::{Path, PathBuf};
use std::fs;
use rusqlite::Connection;
use anyhow::{Context, Result};

#[derive(Debug)]
pub struct DiskInfo {
    pub id: i64,
    pub path: String,
    pub serial: Option<String>,
    pub size: i64,
    pub block_size: i64,
}

pub fn create_pool(db_path: &Path, disk_paths: &[PathBuf]) -> Result<()> {
    if db_path.exists() {
        println!("Database file already exists. Re-initializing pool...");
        fs::remove_file(db_path)
            .with_context(|| format!("Failed to remove existing database at {:?}", db_path))?;
    }

    let mut conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {:?}", db_path))?;

    // Load schema from doc/schema.txt
    let schema = include_str!("../doc/schema.txt");
    conn.execute_batch(schema)
        .context("Failed to initialize database schema")?;

    // Register disks
    let tx = conn.transaction()?;
    for (id, disk_path) in disk_paths.iter().enumerate() {
        let abs_path = fs::canonicalize(disk_path)
            .unwrap_or_else(|_| disk_path.clone());
        
        let metadata = fs::metadata(&abs_path)
            .with_context(|| format!("Failed to read metadata of disk file {:?}", disk_path))?;
        
        let size = metadata.len();
        let path_str = abs_path.to_string_lossy().to_string();
        
        let serial = crate::sys::get_disk_serial(&abs_path)
            .unwrap_or_else(|err| {
                eprintln!("Warning: Failed to retrieve serial number for {:?}: {}", abs_path, err);
                None
            });

        let block_size = crate::sys::get_block_size(&abs_path)
            .unwrap_or_else(|err| {
                eprintln!("Warning: Failed to retrieve block size for {:?}: {}", abs_path, err);
                None
            });

        tx.execute(
            "INSERT INTO disks (id, path, serial, size, block_size) VALUES (?1, ?2, ?3, ?4, ?5)",
            (id as i64 + 1, &path_str, serial, size as i64, block_size),
        )
        .with_context(|| format!("Failed to insert disk {:?}", path_str))?;
    }
    tx.commit()?;
    Ok(())
}

pub fn get_disks(db_path: &Path) -> Result<Vec<DiskInfo>> {
    if !db_path.exists() {
        return Err(anyhow::anyhow!("Database file {:?} does not exist", db_path));
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {:?}", db_path))?;
    
    let mut stmt = conn.prepare("SELECT id, path, serial, size, block_size FROM disks ORDER BY id")?;
    let disk_iter = stmt.query_map((), |row| {
        Ok(DiskInfo {
            id: row.get(0)?,
            path: row.get(1)?,
            serial: row.get(2)?,
            size: row.get(3)?,
            block_size: row.get(4)?,
        })
    })?;

    let mut disks = Vec::new();
    for disk in disk_iter {
        disks.push(disk?);
    }
    Ok(disks)
}
