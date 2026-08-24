use crate::{
    device::{Device, open_device},
    pool::PoolGeometry,
    utils,
};
use anyhow::{Context, Result};
use rusqlite::{Connection, Transaction};
use std::path::{Path, PathBuf};

#[allow(dead_code)]
const BLOCK_SIZE: u64 = 516 * 1024; // 516 KiB

#[allow(dead_code)]
pub struct DiskInfo {
    pub id: i64,
    pub path: PathBuf,
    pub serial: Option<String>,
    pub device_type: String,
    pub size: i64,
    pub block_size: i64,
}

/// Returns the IDs of free superblocks without modifying the database.
/// Returns an error if there are not enough free superblocks available.
pub fn get_free_superblocks_with_tx(tx: &Transaction, required: i64) -> Result<Vec<u64>> {
    // Get the IDs of free superblocks
    let mut superblock_ids = Vec::with_capacity(required as usize);
    let mut stmt =
        tx.prepare("SELECT id FROM superblocks WHERE physical_file_id IS NULL LIMIT ?1")?;
    let superblock_iter = stmt.query_map((required,), |row| row.get(0))?;

    for superblock_id in superblock_iter {
        superblock_ids.push(superblock_id?);
    }

    let available = superblock_ids.len() as i64;
    if available < required {
        return Err(anyhow::anyhow!(
            "Not enough free superblocks. Required: {}, Available: {}",
            required,
            available
        ));
    }

    Ok(superblock_ids)
}

fn string_to_path(s: String) -> PathBuf {
    s.into()
}

pub fn get_disks_with_tx(tx: &Transaction) -> Result<Vec<DiskInfo>> {
    let mut stmt = tx
        .prepare("SELECT id, path, serial, device_type, size, block_size FROM disks ORDER BY id")?;
    let disks = stmt
        .query_map((), |row| {
            Ok(DiskInfo {
                id: row.get(0)?,
                path: string_to_path(row.get(1)?),
                serial: row.get(2)?,
                device_type: row.get(3)?,
                size: row.get(4)?,
                block_size: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, rusqlite::Error>>()?;

    Ok(disks)
}

pub fn open_disks_from_diskinfo(di: &[DiskInfo]) -> Result<Vec<Box<dyn Device>>> {
    let ret = di
        .iter()
        .map(|di| {
            open_device(&di.path)
                .with_context(|| format!("Failed to open disk file {}", di.path.display()))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ret)
}

pub fn open_disks_with_tx(tx: &Transaction) -> Result<Vec<Box<dyn Device>>> {
    open_disks_from_diskinfo(&get_disks_with_tx(tx)?)
}

#[allow(dead_code)]
pub fn get_disks(db_path: &Path) -> Result<Vec<DiskInfo>> {
    if !db_path.exists() {
        return Err(anyhow::anyhow!(
            "Database file {:?} does not exist",
            db_path
        ));
    }
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {:?}", db_path))?;
    get_disks_with_tx(&conn.transaction()?)
}

#[allow(dead_code)]
pub fn get_geometry(db_path: &Path) -> Result<PoolGeometry> {
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {:?}", db_path))?;
    get_geometry_with_tx(&conn.transaction()?)
}

pub fn get_geometry_with_tx(tx: &Transaction) -> Result<PoolGeometry> {
    let disks = get_disks_with_tx(tx)?;
    if disks.is_empty() {
        return Err(anyhow::anyhow!("No disks registered in the pool."));
    }

    let num_disks = disks.len();
    let min_disk_size = disks.iter().map(|d| d.size as u64).min().unwrap_or(0);

    Ok(PoolGeometry::new(num_disks, min_disk_size))
}

/// Checks if there's enough space in the pool for the given file sizes.
/// Returns true if there are enough free superblocks, false otherwise.
///
/// # Arguments
/// * `tx` - Database transaction
/// * `file_sizes` - Slice of file sizes in bytes
pub fn has_enough_space_for_sizes_with_tx(tx: &Transaction, file_sizes: &[u64]) -> Result<bool> {
    let geom = get_geometry_with_tx(tx)?;

    let superblock_size = geom.superblock_size();

    if superblock_size == 0 {
        return Err(anyhow::anyhow!(
            "Invalid pool geometry: logical block size is zero"
        ));
    }

    // Calculate superblocks needed for each file individually, then sum
    let required_superblocks: u64 = file_sizes
        .iter()
        .map(|size| size.div_ceil(superblock_size))
        .sum();

    // Check if there are enough free superblocks
    let mut stmt = tx.prepare("SELECT COUNT(*) FROM superblocks WHERE physical_file_id IS NULL")?;
    let available_superblocks: i64 = stmt.query_row((), |row| row.get(0))?;

    Ok(available_superblocks >= required_superblocks as i64)
}

pub fn get_file_id_with_tx(tx: &Transaction, dst_path: &[&str]) -> Result<Option<i64>> {
    // Get the root directory ID
    let root_id: i64 = tx.query_row("SELECT root FROM fs_root", [], |row| row.get(0))?;

    // Traverse the directory tree to find the parent directory of the file
    let mut current_id = root_id;
    for component in dst_path.iter().take(dst_path.len() - 1) {
        let parent_id: i64 = match tx.query_row(
            "SELECT id, is_dir FROM fs WHERE parent = ?1 AND name = ?2",
            (current_id, *component),
            |row| Ok((row.get(0)?, row.get(1)?)),
        ) {
            Ok((_, 0)) => {
                return Err(anyhow::anyhow!(
                    "Cannot access '{}': a file with the same name already exists",
                    *component
                ));
            }
            Ok((id, _)) => id,
            Err(_) => return Ok(None),
        };
        current_id = parent_id;
    }

    // Check if the file exists in the parent directory
    let file_name = dst_path[dst_path.len() - 1];
    let (id, is_dir): (i64, bool) = match tx.query_row(
        "SELECT id, is_dir FROM fs WHERE parent = ?1 AND name = ?2",
        (current_id, file_name),
        |row| Ok((row.get(0)?, row.get(1)?)),
    ) {
        Ok(result) => result,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(err) => return Err(anyhow::anyhow!("Database query error: {}", err)),
    };

    if is_dir {
        let path = utils::join_path(dst_path);
        Err(anyhow::anyhow!(
            "Path '{path:?}' refers to a directory, not a file"
        ))
    } else {
        Ok(Some(id))
    }
}

/// Checks if a file with the given path exists in the database's filesystem.
/// path_components is a vector of directory/file names representing the path.
pub fn file_exists_with_tx(tx: &Transaction, dst_path: &[&str]) -> Result<bool> {
    Ok(get_file_id_with_tx(tx, dst_path)?.is_some())
}

/// Checks if a file with the given path exists in the database's filesystem.
/// path_components is a vector of directory/file names representing the path.
pub fn file_exists(conn: &mut Connection, dst_path: &[&str]) -> Result<bool> {
    let tx = conn.transaction()?;
    file_exists_with_tx(&tx, dst_path)
}

/// Adds a new physical file record and returns its ID.
fn add_physical_file(tx: &rusqlite::Transaction, size: u64, hash: &str) -> Result<i64> {
    tx.execute(
        "INSERT INTO physical_files (size, hash) VALUES (?1, ?2)",
        (size as i64, hash),
    )?;

    Ok(tx.last_insert_rowid())
}

/// Updates superblocks with physical_file_id and file_order.
fn assign_superblocks(
    tx: &rusqlite::Transaction,
    physical_file_id: i64,
    superblock_ids: &[u64],
) -> Result<()> {
    for (order, &superblock_id) in superblock_ids.iter().enumerate() {
        tx.execute(
            "UPDATE superblocks SET physical_file_id = ?1, file_order = ?2 WHERE id = ?3",
            (physical_file_id, order as i64, superblock_id as i64),
        )?;
    }
    Ok(())
}

/// Creates intermediate directories in the filesystem and returns the parent ID.
pub fn ensure_path(tx: &rusqlite::Transaction, path: &[&str]) -> Result<i64> {
    // Get the root directory ID
    let root_id: i64 = tx.query_row("SELECT root FROM fs_root", [], |row| row.get(0))?;

    let mut current_id = root_id;

    // Create intermediate directories if they don't exist
    for component in path.iter().take(path.len() - 1) {
        let parent_id = match tx.query_row(
            "SELECT id, is_dir FROM fs WHERE parent = ?1 AND name = ?2",
            (current_id, *component),
            |row| Ok((row.get(0)?, row.get(1)?)),
        ) {
            Ok((_, 0)) => {
                return Err(anyhow::anyhow!(
                    "Cannot create directory '{}': a file with the same name already exists",
                    *component
                ));
            }
            Ok((id, _)) => id,
            Err(_) => {
                // Entry doesn't exist, create directory
                tx.execute(
                    "INSERT INTO fs (name, parent, is_dir, is_virtual) VALUES (?1, ?2, 1, 0)",
                    (*component, current_id),
                )?;
                tx.last_insert_rowid()
            }
        };
        current_id = parent_id;
    }

    Ok(current_id)
}

pub fn commit_file_with_tx(
    tx: &Transaction,
    dst_path: &[&str],
    file_size: u64,
    file_hash: &str,
    allocated_ids: &[u64],
) -> Result<()> {
    // Add physical file record
    let physical_file_id = add_physical_file(tx, file_size, file_hash)
        .context("Failed to add physical file record")?;

    // Assign superblocks to this file
    assign_superblocks(tx, physical_file_id, allocated_ids)
        .context("Failed to assign superblocks")?;

    // Create intermediate directories and add file entry
    let parent_id = ensure_path(tx, dst_path).context("Failed to ensure path")?;
    tx.execute(
        "INSERT INTO fs (name, parent, is_dir, is_virtual, file_id, all_or_nothing) VALUES (?1, ?2, 0, 0, ?3, 1)",
        (&dst_path[dst_path.len() - 1], parent_id, physical_file_id),
    )?;

    Ok(())
}

fn delete_physical_file_with_tx(
    tx: &Transaction,
    file_id: i64,
    physical_file_id: i64,
) -> Result<()> {
    // Delete the file entry from fs
    tx.execute("DELETE FROM fs WHERE id = ?1", (file_id,))?;

    // Check if any other files reference this physical file
    let ref_count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM fs WHERE file_id = ?1",
        (file_id,),
        |row| row.get(0),
    )?;

    if ref_count == 0 {
        // No other files reference this physical file, delete it
        tx.execute(
            "DELETE FROM physical_files WHERE id = ?1",
            (physical_file_id,),
        )?;

        // Reset all superblocks that were assigned to this file
        tx.execute(
            "UPDATE superblocks SET physical_file_id = NULL, file_order = NULL WHERE physical_file_id = ?1",
            (physical_file_id,),
        )?;
    }

    Ok(())
}

/// Deletes a file from the pool.
/// If no other files reference the same physical file, it will also delete
/// the physical file record and reset the superblocks.
/// Returns an error if the file is marked as virtual.
pub fn delete_file_with_tx(tx: &Transaction, file_id: i64) -> Result<()> {
    let (is_virtual, physical_file_id): (i64, i64) = tx.query_row(
        "SELECT is_virtual, file_id FROM fs WHERE id = ?1",
        (file_id,),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    if is_virtual != 0 {
        Err(anyhow::anyhow!("Cannot delete virtual file"))
    } else {
        delete_physical_file_with_tx(tx, file_id, physical_file_id)
    }
}

/// Deletes a file from the pool by path.
/// If no other files reference the same physical file, it will also delete
/// the physical file record and reset the superblocks.
/// Returns an error if the file is marked as virtual.
#[allow(dead_code)]
pub fn delete_file(tx: &Transaction, path_components: &[&str]) -> Result<()> {
    let file_id = get_file_id_with_tx(tx, path_components).with_context(|| {
        format!(
            "Failed to find file at path {:?}",
            utils::join_path(path_components)
        )
    })?;

    let file_id = match file_id {
        Some(id) => id,
        None => {
            // File doesn't exist, nothing to delete
            return Ok(());
        }
    };

    delete_file_with_tx(tx, file_id)
}
