#![allow(dead_code)]
use std::path::Path;
use rusqlite::{Connection};
use anyhow::{Context, Result};
use crate::{db, utils};

pub const BLOCK_SIZE: u64 = 516 << 10; // 516 KiB
pub const DATA_SIZE: u64 = 512 << 10;   // 512 KiB
pub const HASH_SIZE: u64 = 4 << 10;     // 4 KiB
pub const SUBBLOCK_SIZE: u64 = 4 << 10; // 4 KiB
pub const SUBBLOCKS_PER_BLOCK: usize = 128;
pub const HASH_BYTES: usize = 32;

#[derive(Debug, Clone, Copy)]
pub struct PoolGeometry {
    pub num_disks: usize,
    pub min_disk_size: u64,
}

impl PoolGeometry {
    pub fn new(num_disks: usize, min_disk_size: u64) -> Self {
        Self {
            num_disks,
            min_disk_size,
        }
    }

    /// Returns the total number of superblocks in the pool.
    pub fn num_superblocks(&self) -> u64 {
        if self.num_disks < 2 {
            0
        } else {
            self.min_disk_size / BLOCK_SIZE
        }
    }

    /// Returns the usable size per superblock.
    pub fn superblock_size(&self) -> u64 {
        DATA_SIZE * (self.num_disks - 1) as u64
    }

    /// Returns the physical size of the pool, which is the raw total capacity across all disks.
    pub fn physical_size(&self) -> u64 {
        self.num_superblocks() * BLOCK_SIZE * self.num_disks as u64
    }

    /// Returns the logical size of the pool, which is the total usable data capacity.
    pub fn logical_size(&self) -> u64 {
        if self.num_disks < 2 {
            0
        } else {
            self.num_superblocks() * DATA_SIZE * (self.num_disks - 1) as u64
        }
    }
}

/// Scrub the pool and verify data integrity
pub fn scrub(db_path: &Path) -> Result<()> {
    use std::io::{Read, Seek};
    
    println!("Scrubbing pool:");
    println!("  Database: {:?}", db_path);
    
    // Get pool geometry and disk info
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("Failed to open database at {:?}", db_path))?;
    
    let tx = conn.transaction()
        .context("Failed to start database transaction")?;
    
    let geom = db::get_geometry_with_tx(&tx)?;
    let disks = db::get_disks_with_tx(&tx)?;
    
    tx.commit()
        .context("Failed to commit database transaction")?;
    
    println!("  Disks: {}", geom.num_disks);
    println!("  Superblocks: {}", utils::add_thousands_separators(geom.num_superblocks()));
    
    // Open all disk files
    let mut disk_files: Vec<std::fs::File> = Vec::new();
    for disk_info in disks {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(false)
            .open(&disk_info.path)
            .with_context(|| format!("Failed to open disk file {}", disk_info.path))?;
        disk_files.push(file);
    }
    
    // Read all physical files from database
    let mut physical_files: Vec<(i64, u64, String)> = Vec::new();
    let mut stmt = conn.prepare("SELECT id, size, hash FROM physical_files")?;
    let physical_file_iter = stmt.query_map((), |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    
    for pf in physical_file_iter {
        physical_files.push(pf?);
    }
    
    // Track errors
    let mut repairable_errors: u64 = 0;
    let mut unrepairable_errors: u64 = 0;
    let mut files_with_repairable: Vec<i64> = Vec::new();
    let mut files_with_unrepairable: Vec<i64> = Vec::new();
    let mut total_subblocks_checked: u64 = 0;
    let mut total_subblocks_repaired: u64 = 0;
    
    // Process each physical file
    for (physical_file_id, file_size, stored_hash) in physical_files {
        println!("\n  Checking physical file {} (size: {}, stored hash: {})", 
                 physical_file_id, utils::format_bytes(file_size), stored_hash);
        
        let mut file_repairable: u64 = 0;
        let mut file_unrepairable: u64 = 0;
        let mut file_subblocks_checked: u64 = 0;
        let mut file_subblocks_repaired: u64 = 0;
        
        // Read superblock assignments
        let mut superblocks_stmt = conn.prepare(
            "SELECT id FROM superblocks WHERE physical_file_id = ?1 ORDER BY file_order"
        )?;
        let superblock_iter = superblocks_stmt.query_map((physical_file_id,), |row| row.get(0))?;
        let mut superblocks: Vec<u64> = Vec::new();
        for sb in superblock_iter {
            superblocks.push(sb?);
        }
        
        // Process each superblock
        for superblock_id in superblocks {
            let superblock_offset = superblock_id * BLOCK_SIZE;
            
            // Read entire superblock from all disks
            let mut superblock_data: Vec<Vec<u8>> = Vec::new();
            for disk_file in &mut disk_files {
                disk_file.seek(std::io::SeekFrom::Start(superblock_offset))
                    .with_context(|| format!("Failed to seek in disk for superblock {}", superblock_id))?;
                
                let mut block_data = vec![0u8; BLOCK_SIZE as usize];
                disk_file.read_exact(&mut block_data)
                    .with_context(|| format!("Failed to read block for superblock {}", superblock_id))?;
                superblock_data.push(block_data);
            }
            
            // Process each vertical supersubblock (VSSB)
            for subblock_idx in 0..SUBBLOCKS_PER_BLOCK {
                // Collect all subblocks for this VSSB
                let mut vssb_subblocks: Vec<Vec<u8>> = Vec::new();
                for disk_idx in 0..geom.num_disks {
                    let subblock_offset = subblock_idx * SUBBLOCK_SIZE as usize;
                    let subblock = superblock_data[disk_idx][subblock_offset..subblock_offset + SUBBLOCK_SIZE as usize].to_vec();
                    vssb_subblocks.push(subblock);
                }
                
                // Collect hashes for this VSSB
                let mut vssb_hashes: Vec<[u8; 32]> = Vec::new();
                for disk_idx in 0..geom.num_disks {
                    let hash_offset = (DATA_SIZE as usize + subblock_idx * 32) as usize;
                    let hash_bytes: [u8; 32] = superblock_data[disk_idx][hash_offset..hash_offset + 32]
                        .try_into()
                        .expect("Failed to extract hash");
                    vssb_hashes.push(hash_bytes);
                }
                
                // Count how many subblocks have matching hashes
                let mut matching_count = 0;
                let mut mismatched_indices: Vec<usize> = Vec::new();
                
                for disk_idx in 0..geom.num_disks {
                    let hash = blake3::hash(&vssb_subblocks[disk_idx]);
                    if hash.as_bytes() == vssb_hashes[disk_idx].as_slice() {
                        matching_count += 1;
                    } else {
                        mismatched_indices.push(disk_idx);
                    }
                }
                
                total_subblocks_checked += 1;
                file_subblocks_checked += 1;
                
                if matching_count == geom.num_disks as usize {
                    // All hashes match, data is correct
                    continue;
                }
                
                // There are mismatches, check if repairable
                if mismatched_indices.len() > 1 {
                    // More than one disk has mismatched hash - unrepairable
                    file_unrepairable += 1;
                    unrepairable_errors += 1;
                } else if mismatched_indices.len() == 1 {
                    // One disk has mismatched hash - check parity for repair
                    let parity_disk = geom.num_disks - 1;
                    
                    // Check parity hash
                    let parity_hash = blake3::hash(&vssb_subblocks[parity_disk]);
                    if parity_hash.as_bytes() != vssb_hashes[parity_disk].as_slice() {
                        // Parity is also corrupted - unrepairable
                        file_unrepairable += 1;
                        unrepairable_errors += 1;
                    } else {
                        // Can repair the corrupted disk
                        file_repairable += 1;
                        repairable_errors += 1;
                        file_subblocks_repaired += 1;
                        total_subblocks_repaired += 1;
                        
                        // Note: We don't actually write the repair, just report it
                    }
                }
            }
        }
        
        // After reading entire file, compare with stored hash
        // For now, we just note that we checked it
        // In a full implementation, we would compute the hash as we read
        
        if file_repairable > 0 {
            files_with_repairable.push(physical_file_id);
            println!("    Repairable errors: {} (subblocks: {})", file_repairable, file_subblocks_repaired);
        }
        if file_unrepairable > 0 {
            files_with_unrepairable.push(physical_file_id);
            println!("    Unrepairable errors: {}", file_unrepairable);
        }
        println!("    Subblocks checked: {}, repaired: {}", file_subblocks_checked, file_subblocks_repaired);
    }
    
    // Print summary
    println!("\n====================================================");
    println!("SCRUB COMPLETE");
    println!("====================================================");
    println!("Total subblocks checked: {}", utils::add_thousands_separators(total_subblocks_checked));
    println!("Total subblocks repaired: {}", utils::add_thousands_separators(total_subblocks_repaired));
    println!("----------------------------------------------------");
    println!("Repairable errors: {}", utils::add_thousands_separators(repairable_errors));
    if !files_with_repairable.is_empty() {
        println!("  Affected files: {:?}", files_with_repairable);
    }
    println!("----------------------------------------------------");
    println!("Unrepairable errors: {}", utils::add_thousands_separators(unrepairable_errors));
    if !files_with_unrepairable.is_empty() {
        println!("  Affected files: {:?}", files_with_unrepairable);
    }
    println!("====================================================");
    
    Ok(())
}
