use std::path::PathBuf;
use anyhow::{Context, Result};
use crate::db;
use crate::pool::PoolGeometry;
use crate::utils;
use blake3::Hasher;
use std::io::{Read, Seek};

/// Adds a file to the pool after verifying it doesn't already exist.
pub fn add_file(
    db_path: &PathBuf,
    file_path: &PathBuf,
    path_components: &[&str],
) -> Result<()> {

    // Get pool geometry from database
    let disks = db::get_disks(db_path)
        .context("Failed to retrieve pool information")?;
    
    if disks.is_empty() {
        return Err(anyhow::anyhow!("No disks registered in the pool."));
    }

    let num_disks = disks.len();
    let min_disk_size = disks.iter().map(|d| d.size as u64).min().unwrap_or(0);
    let geom = PoolGeometry::new(num_disks, min_disk_size);

    let file_size = std::fs::metadata(file_path)
        .with_context(|| format!("Failed to read file metadata {:?}", file_path))?
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
    let allocated_ids = db::get_free_superblocks(db_path, required_superblocks as i64)
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
    let mut file = std::fs::File::open(file_path)
        .with_context(|| format!("Failed to open file {:?}", file_path))?;

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
            .with_context(|| format!("Failed to seek in file {file_path:?}"))?;
        file.read_exact(&mut superblock_data)
            .with_context(|| format!("Failed to read file {file_path:?}"))?;
        
        // Update hasher with superblock data
        hasher.update(&superblock_data);
        
        crate::disk::write_superblock(superblock_id, superblock_data, &geom, &mut disk_files)?;
    }
    
    // Finalize with database transaction
    let final_hash = hex::encode(hasher.finalize().as_bytes());
    println!("  File hash: {final_hash}");
    
    db::commit_file(db_path, &path_components, file_size, &final_hash, &allocated_ids)
        .context("Failed to commit file to database")?;
    
    Ok(())
}
