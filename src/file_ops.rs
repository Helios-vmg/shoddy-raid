use std::path::PathBuf;
use anyhow::{Context, Result};
use rusqlite::Transaction;
use crate::db;
use crate::pool::PoolGeometry;
use crate::utils;
use blake3::Hasher;
use std::io::{Read, Seek};

/// Adds a file to the pool after verifying it doesn't already exist.
pub fn add_file(
    tx: &Transaction,
    real_file_path: &PathBuf,
    path_components: &[&str],
) -> Result<()> {

    // Get pool geometry from database
    let disks = db::get_disks_with_tx(tx)
        .context("Failed to retrieve pool information")?;
    
    if disks.is_empty() {
        return Err(anyhow::anyhow!("No disks registered in the pool."));
    }

    let num_disks = disks.len();
    let min_disk_size = disks.iter().map(|d| d.size as u64).min().unwrap_or(0);
    let geom = PoolGeometry::new(num_disks, min_disk_size);

    let file_size = std::fs::metadata(real_file_path)
        .with_context(|| format!("Failed to read file metadata {:?}", real_file_path))?
        .len();
    println!("  File size: {}", utils::format_bytes(file_size));

    // Calculate number of superblocks needed
    let logical_block_size = geom.logical_size();
    if logical_block_size == 0 {
        return Err(anyhow::anyhow!("Invalid pool geometry: logical block size is zero"));
    }
    let required_superblocks = (file_size + logical_block_size - 1) / logical_block_size;
    println!("  Superblocks needed: {}", utils::add_thousands_separators(required_superblocks));

    // Allocate superblocks from the pool
    let allocated_ids = db::get_free_superblocks_with_tx(tx, required_superblocks as i64)
        .with_context(|| format!("Failed to allocate {} superblocks", required_superblocks))?;

    // Open all disk files
    let mut disk_files: Vec<std::fs::File> = Vec::new();
    for disk_info in &disks {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&disk_info.path)
            .with_context(|| format!("Failed to open disk file {}", disk_info.path))?;
        disk_files.push(file);
    }

    // Open the file for reading in chunks
    let mut file = std::fs::File::open(real_file_path)
        .with_context(|| format!("Failed to open file {:?}", real_file_path))?;

    // Create hasher for whole-file hash
    let mut hasher = Hasher::new();

    // Process each superblock
    for (i, &superblock_id) in allocated_ids.iter().enumerate() {
        let start = (i as u64) * logical_block_size;
        if start >= file_size {
            continue;
        }
        let end = std::cmp::min(start + logical_block_size, file_size);
        
        let superblock_size = (end - start) as usize;
        
        println!("  Processing superblock {i}: {superblock_size} bytes (bytes {start}-{end})");
        
        // Read superblock data from file
        let mut superblock_data = vec![0u8; superblock_size];
        file.seek(std::io::SeekFrom::Start(start))
            .with_context(|| format!("Failed to seek in file {real_file_path:?}"))?;
        file.read_exact(&mut superblock_data)
            .with_context(|| format!("Failed to read file {real_file_path:?}"))?;
        
        // Update hasher with superblock data
        hasher.update(&superblock_data);
        
        crate::disk::write_superblock(superblock_id, superblock_data, &geom, &mut disk_files)?;
    }
    
    // Finalize with database transaction
    let final_hash = hex::encode(hasher.finalize().as_bytes());
    println!("  File hash: {final_hash}");
    
    db::commit_file_with_tx(tx, &path_components, file_size, &final_hash, &allocated_ids)
        .context("Failed to commit file to database")?;
    
    Ok(())
}

/// Deletes a file from the pool.
/// If no other files reference the same physical file, it will also delete
/// the physical file record and reset the superblocks.
/// Returns an error if the file is marked as virtual.
pub fn delete_file(
    tx: &Transaction,
    path_components: &[&str],
) -> Result<()> {
    let file_id = db::get_file_id_with_tx(&tx, path_components)
        .with_context(|| format!("Failed to find file at path {:?}", utils::join_path(path_components)))?;

    let file_id = match file_id {
        Some(id) => id,
        None => {
            // File doesn't exist, nothing to delete
            return Ok(());
        }
    };

    db::delete_file_with_tx(&tx, file_id)
}

/// Replaces an existing file in the pool with a new one.
pub fn replace_file(
    tx: &Transaction,
    real_file_path: &PathBuf,
    path_components: &[&str],
) -> Result<()> {
    delete_file(tx, path_components)?;
    add_file(tx, real_file_path, path_components)
}

/// Recursively adds a directory to the pool.
/// 
/// # Arguments
/// * `tx` - Database transaction
/// * `dir_path` - Path to the local directory to add
/// * `dst_path` - Path components where to store in the pool
/// * `force` - If true, overwrite existing files; otherwise fail if any destination exists
pub fn add_directory(
    tx: &Transaction,
    dir_path: &PathBuf,
    dst_path: &[&str],
    force: bool,
) -> Result<()> {
    use std::fs;
    use std::path::Path;

    // Get pool geometry from database
    let disks = db::get_disks_with_tx(tx)
        .context("Failed to retrieve pool information")?;
    
    if disks.is_empty() {
        return Err(anyhow::anyhow!("No disks registered in the pool."));
    }

    let num_disks = disks.len();
    let min_disk_size = disks.iter().map(|d| d.size as u64).min().unwrap_or(0);
    let geom = PoolGeometry::new(num_disks, min_disk_size);

    // Step 1: Recursively list all directory entries with their relative paths and sizes
    let mut entries: Vec<(PathBuf, u64, bool)> = Vec::new(); // (relative_path, size, is_dir)
    
    fn collect_entries(
        base_path: &Path,
        current_path: &Path,
        entries: &mut Vec<(PathBuf, u64, bool)>,
    ) -> Result<()> {
        let full_path = base_path.join(current_path);
        
        for entry in fs::read_dir(&full_path)
            .with_context(|| format!("Failed to read directory {:?}", full_path))? 
        {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            let rel_path = path.strip_prefix(base_path)
                .unwrap_or(&path)
                .to_path_buf();
            
            if file_type.is_dir() {
                collect_entries(base_path, &rel_path, entries)?;
                entries.push((rel_path, 0, true));
            } else if file_type.is_file() {
                let size = fs::metadata(&path)?.len();
                entries.push((rel_path, size, false));
            }
        }
        
        Ok(())
    }
    
    collect_entries(dir_path, &PathBuf::new(), &mut entries)
        .context("Failed to collect directory entries")?;
    
    // Step 2: Sum all sizes and ensure there's enough space in the pool
    let logical_block_size = geom.logical_size();
    if logical_block_size == 0 {
        return Err(anyhow::anyhow!("Invalid pool geometry: logical block size is zero"));
    }
    // Calculate superblocks needed for each file individually, then sum
    let required_superblocks: u64 = entries.iter()
        .filter(|(_, size, _)| *size > 0) // Only count files, not directories
        .map(|(_, size, _)| (*size + logical_block_size - 1) / logical_block_size)
        .sum();
    
    // Get available superblocks
    let available_superblocks = db::get_free_superblocks_with_tx(tx, required_superblocks as i64)
        .with_context(|| format!("Not enough space in pool. Required: {}, Available: {}", required_superblocks, 0))?;
    
    if (available_superblocks.len() as i64) < (required_superblocks as i64) {
        return Err(anyhow::anyhow!(
            "Not enough space in pool. Required: {} superblocks, Available: {}",
            required_superblocks,
            available_superblocks.len() as i64
        ));
    }
    
    // Step 3: Determine destination location of each entry
    // Step 4: Check if destinations exist (unless force is specified)
    for (rel_path, _, _) in &entries {
        let mut full_dst_path = dst_path.to_vec();
        for component in rel_path.components() {
            let str_val = component.as_os_str().to_str().unwrap_or("");
            full_dst_path.push(str_val);
        }
        
        if force{
            continue;
        }

        if db::file_exists_with_tx(tx, &full_dst_path)? {
            let path_str = utils::join_path(&full_dst_path);
            return Err(anyhow::anyhow!(
                "'{}' already exists in the pool. Use --force to overwrite.",
                path_str.display()
            ));
        }
    }
    
    // Step 5: For every file, create or replace as necessary
    for (rel_path, _, is_dir) in entries {
        if is_dir {
            // Create directory entry in database
            let mut full_dst_path = dst_path.to_vec();
            for component in rel_path.components() {
                let str_val = component.as_os_str().to_str().unwrap_or("");
                full_dst_path.push(str_val);
            }
            
            db::ensure_path(tx, &full_dst_path)
                .with_context(|| format!("Failed to create directory {:?}", utils::join_path(&full_dst_path)))?;
        } else {
            // Add or replace file
            let mut full_dst_path = dst_path.to_vec();
            for component in rel_path.components() {
                let str_val = component.as_os_str().to_str().unwrap_or("");
                full_dst_path.push(str_val);
            }
            
            let src_path = dir_path.join(&rel_path);
            
            if db::file_exists_with_tx(tx, &full_dst_path)? {
                if !force {
                    return Err(anyhow::anyhow!(
                        "File '{}' unexpectedly exists. Use --force to overwrite.",
                        utils::join_path(&full_dst_path).display()
                    ));
                }
                replace_file(tx, &src_path, &full_dst_path)?;
            } else {
                add_file(tx, &src_path, &full_dst_path)?;
            }
        }
    }
    
    Ok(())
}
