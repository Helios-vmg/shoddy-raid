use std::path::{Path, PathBuf};
use std::fs;
use rusqlite::{
    Connection,
    OptionalExtension,
};
use anyhow::{Context, Result};

const BLOCK_SIZE: u64 = 516 * 1024; // 516 KiB

#[derive(Debug)]
pub struct DiskInfo {
    pub id: i64,
    pub path: String,
    pub serial: Option<String>,
    pub size: i64,
    pub block_size: i64,
}

/// Registers disks in the database and returns the minimum disk size.
fn register_disks(
    tx: &rusqlite::Transaction,
    disk_paths: &[PathBuf],
) -> Result<u64> {
    let mut disk_sizes = Vec::new();

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
            (id as i64 + 1, &path_str, &serial, size as i64, block_size),
        )
        .with_context(|| format!("Failed to insert disk {:?}", path_str))?;

        disk_sizes.push(size);
    }

    let min_disk_size = disk_sizes.iter().min().copied().unwrap_or(0);
    Ok(min_disk_size)
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

    // Register disks and get minimum size
    let tx = conn.transaction()?;
    let min_disk_size = register_disks(&tx, disk_paths)?;
    tx.commit()?;

    // Calculate valid superblocks based on smallest disk
    let num_superblocks = min_disk_size / BLOCK_SIZE;

    // Populate superblocks table with all valid superblock IDs
    let tx = conn.transaction()?;
    for superblock_id in 0..num_superblocks {
        tx.execute(
            "INSERT INTO superblocks (id, physical_file_id, file_order, read_errors) VALUES (?1, NULL, NULL, 0)",
            (superblock_id,),
        )
        .with_context(|| format!("Failed to insert superblock {}", superblock_id))?;
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

/// Returns the IDs of free superblocks without modifying the database.
/// Returns an error if there are not enough free superblocks available.
pub fn get_free_superblocks(
    db_path: &Path,
    required: i64,
) -> Result<Vec<u64>> {
    if !db_path.exists() {
        return Err(anyhow::anyhow!("Database file {:?} does not exist", db_path));
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {:?}", db_path))?;

    // Get the IDs of free superblocks
    let mut superblock_ids = Vec::with_capacity(required as usize);
    let mut stmt = conn.prepare(
        "SELECT id FROM superblocks WHERE physical_file_id IS NULL LIMIT ?1"
    )?;
    let superblock_iter = stmt.query_map((required,), |row| row.get(0))?;
    
    for superblock_id in superblock_iter {
        superblock_ids.push(superblock_id?);
    }

    let available = superblock_ids.len() as i64;
    if available < required {
        return Err(anyhow::anyhow!(
            "Not enough free superblocks. Required: {}, Available: {}",
            required, available
        ));
    }

    Ok(superblock_ids)
}

/// Checks if a file with the given path exists in the database's filesystem.
/// path_components is a vector of directory/file names representing the path.
pub fn file_exists(db_path: &Path, path_components: &[&str]) -> Result<bool> {
    if !db_path.exists() {
        return Err(anyhow::anyhow!("Database file {:?} does not exist", db_path));
    }

    // Empty path refers to root directory, not a file
    if path_components.is_empty() {
        return Err(anyhow::anyhow!("Path refers to a directory"));
    }

    let conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {:?}", db_path))?;

    // Get the root directory ID
    let root_id: i64 = conn.query_row("SELECT root FROM fs_root", [], |row| row.get(0))?;

    // Traverse the directory tree to find the parent directory of the file
    let mut current_id = root_id;
    for component in path_components.iter().take(path_components.len() - 1) {
        let parent_id: i64 = match conn.query_row(
            "SELECT id FROM fs WHERE parent = ?1 AND name = ?2 AND is_dir = 1",
            (current_id, *component),
            |row| row.get(0),
        ) {
            Ok(id) => id,
            Err(_) => return Ok(false), // Directory not found
        };
        current_id = parent_id;
    }

    // Check if the file exists in the parent directory
    let file_name = path_components[path_components.len() - 1];
    let is_dir: Option<i64> = conn.query_row(
        "SELECT is_dir FROM fs WHERE parent = ?1 AND name = ?2",
        (current_id, file_name),
        |row| row.get(0),
    ).optional()?;
    
    // If no row returned, file doesn't exist
    match is_dir {
        Some(0) => Ok(true),
        None => Ok(false),
        _ => Err(anyhow::anyhow!("Path '{}' refers to a directory, not a file", file_name))
    }
}
