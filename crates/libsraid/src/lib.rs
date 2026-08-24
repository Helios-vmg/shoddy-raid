mod db;
mod disk;
mod file_ops;
mod fs;
mod pool;
mod sys;
mod utils;

use std::path::{Path, PathBuf};
use std::time::Instant;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::io::{Read, Seek, Write};

pub const BLOCK_SIZE: u64 = 516 * 1024; // 516 KiB
pub const DATA_SIZE: u64 = 512 * 1024;   // 512 KiB
pub const HASH_SIZE: u64 = 4 * 1024;     // 4 KiB
pub const SUBBLOCK_SIZE: u64 = 4 * 1024; // 4 KiB
pub const SUBBLOCKS_PER_BLOCK: usize = 128;
pub const HASH_BYTES: usize = 32;

/// Represents disk information
#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub path: PathBuf,
    pub size: u64,
    pub block_size: u64,
    pub serial: Option<String>,
}

/// Result of a scrub operation
#[derive(Debug)]
pub struct ScrubResult {
    pub repairable_or_repaired_files: u64,
    pub repairable_or_repaired_blocks: u64,
    pub damaged_files: Vec<PathBuf>,
    pub raw_bytes_read: u64,
    pub elapsed_seconds: f64,
}

/// Result of an info operation
#[derive(Debug)]
pub struct InfoResult {
    pub total_disks: u64,
    pub smallest_disk_raw_bytes: u64,
    pub raw_pool_bytes: u64,
    pub usable_pool_bytes: u64,
    pub efficiency: f64,
    pub total_superblocks: u64,
    pub raw_block_size: u64,
    pub raw_superblock_size: u64,
    pub logical_superblock_size: u64,
    pub disks: Vec<DiskInfo>,
}

/// LibSRaid - Core library for managing a shoddy RAID pool
pub struct LibSRaid {
    #[allow(dead_code)]
    db_path: PathBuf,
    conn: Connection,
}

impl LibSRaid {
    /// Creates a new pool with the given database path and disk paths
    pub fn create(db_path: &Path, disk_paths: &[PathBuf]) -> Result<Self> {
        if disk_paths.is_empty() {
            return Err(anyhow::anyhow!("At least 2 disks are required to create a RAID pool."));
        }
        if disk_paths.len() < 2 {
            return Err(anyhow::anyhow!("A RAID pool requires at least 2 disks (including 1 parity disk)."));
        }

        if db_path.exists() {
            std::fs::remove_file(db_path)
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

        Ok(Self {
            db_path: db_path.to_path_buf(),
            conn,
        })
    }

    /// Opens an existing pool
    pub fn open(db_path: &Path) -> Result<Self> {
        if !db_path.exists() {
            return Err(anyhow::anyhow!("Database file {:?} does not exist", db_path));
        }

        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open database at {:?}", db_path))?;

        Ok(Self {
            db_path: db_path.to_path_buf(),
            conn,
        })
    }

    /// Adds a file to the pool
    pub fn add_file(&mut self, source: &Path, destination: &Path, force: bool) -> Result<()> {
        let path_components: Vec<&str> = destination
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();

        let source_pathbuf = source.to_path_buf();
        if db::file_exists(&mut self.conn, &path_components)? {
            if !force {
                return Err(anyhow::anyhow!(
                    "File '{}' already exists in the pool",
                    destination.display()
                ));
            }
            file_ops::replace_file(&mut self.conn, &source_pathbuf, &path_components)?;
        } else {
            file_ops::add_file(&mut self.conn, &source_pathbuf, &path_components)?;
        }
        Ok(())
    }

    /// Adds a directory to the pool
    pub fn add_directory(&mut self, source: &Path, destination: &Path, force: bool) -> Result<()> {
        let path_components: Vec<&str> = destination
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect();

        let source_pathbuf = source.to_path_buf();
        file_ops::add_directory(&mut self.conn, &source_pathbuf, &path_components, force)
            .context("Failed to add directory to pool")?;
        Ok(())
    }

    /// Scrubs the pool and verifies data integrity
    pub fn scrub(&mut self, dry_run: bool) -> Result<ScrubResult> {
        let start_time = Instant::now();

        let tx = self.conn.transaction()
            .context("Failed to start database transaction")?;

        let geom = db::get_geometry_with_tx(&tx)?;
        let disks = db::get_disks_with_tx(&tx)?;

        tx.commit()
            .context("Failed to commit database transaction")?;

        // Open all disk files
        let mut disk_files: Vec<std::fs::File> = Vec::new();
        for disk_info in &disks {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(!dry_run)
                .open(&disk_info.path)
                .with_context(|| format!("Failed to open disk file {}", disk_info.path))?;
            disk_files.push(file);
        }

        // Read all physical files from database
        let mut physical_files: Vec<(i64, u64, String)> = Vec::new();
        let mut stmt = self.conn.prepare("SELECT id, size, hash FROM physical_files")?;
        let physical_file_iter = stmt.query_map((), |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;

        for pf in physical_file_iter {
            physical_files.push(pf?);
        }

        // Track errors
        let mut repairable_or_repaired_files: u64 = 0;
        let mut repairable_or_repaired_blocks: u64 = 0;
        let mut damaged_files: Vec<PathBuf> = Vec::new();
        let mut raw_bytes_read: u64 = 0;

        // Process each physical file
        for (physical_file_id, _file_size, _stored_hash) in &physical_files {
            let _file_repairable: u64 = 0;
            let mut file_damaged = false;

            // Read superblock assignments
            let mut superblocks_stmt = self.conn.prepare(
                "SELECT id FROM superblocks WHERE physical_file_id = ?1 ORDER BY file_order"
            )?;
            let superblock_iter = superblocks_stmt.query_map((physical_file_id,), |row| row.get(0))?;
            let mut superblocks: Vec<u64> = Vec::new();
            for sb in superblock_iter {
                superblocks.push(sb?);
            }

            // Process each superblock
            for superblock_id in &superblocks {
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
                    raw_bytes_read += BLOCK_SIZE;
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

                    if matching_count == geom.num_disks as usize {
                        // All hashes match, data is correct
                        continue;
                    }

                    // There are mismatches, check if repairable
                    if mismatched_indices.len() > 1 {
                        // More than one disk has mismatched hash - unrepairable
                        file_damaged = true;
                    } else if mismatched_indices.len() == 1 {
                        // One disk has mismatched hash - check parity for repair
                        let parity_disk = geom.num_disks - 1;

                        // Check parity hash
                        let parity_hash = blake3::hash(&vssb_subblocks[parity_disk]);
                        if parity_hash.as_bytes() != vssb_hashes[parity_disk].as_slice() {
                            // Parity is also corrupted - unrepairable
                            file_damaged = true;
                        } else if !dry_run {
                            // Can repair using parity
                            let damaged_disk = mismatched_indices[0];

                            // Calculate correct data using parity
                            let mut corrected = [0u8; SUBBLOCK_SIZE as usize];
                            for disk in 0..geom.num_disks {
                                if disk != damaged_disk && disk != parity_disk {
                                    let data = &vssb_subblocks[disk];
                                    for j in 0..SUBBLOCK_SIZE as usize {
                                        corrected[j] ^= data[j];
                                    }
                                }
                            }

                            // Write corrected data to disk
                            let corrected_data = &corrected[..];
                            let disk_file = &mut disk_files[damaged_disk];
                            let seek_pos = (superblock_offset as usize + subblock_idx * SUBBLOCK_SIZE as usize) as u64;
                            disk_file.seek(std::io::SeekFrom::Start(seek_pos))
                                .with_context(|| format!("Failed to seek in disk {} for repair", damaged_disk))?;
                            disk_file.write_all(corrected_data)
                                .with_context(|| format!("Failed to write corrected data to disk {}", damaged_disk))?;

                            repairable_or_repaired_blocks += 1;
                        }
                    }
                }
            }

            if file_damaged {
                damaged_files.push(PathBuf::from(format!("physical_file_{}", physical_file_id)));
                repairable_or_repaired_files += 1;
            }
        }

        let elapsed_seconds = start_time.elapsed().as_secs_f64();

        Ok(ScrubResult {
            repairable_or_repaired_files,
            repairable_or_repaired_blocks,
            damaged_files,
            raw_bytes_read,
            elapsed_seconds,
        })
    }

    /// Returns information about the pool
    pub fn info(&mut self) -> Result<InfoResult> {
        let tx = self.conn.transaction()
            .context("Failed to start database transaction")?;

        let geom = db::get_geometry_with_tx(&tx)?;
        let disks = db::get_disks_with_tx(&tx)?;

        tx.commit()
            .context("Failed to commit database transaction")?;

        let total_disks = geom.num_disks as u64;
        let smallest_disk_raw_bytes = geom.min_disk_size;
        let raw_pool_bytes = geom.physical_size();
        let usable_pool_bytes = geom.logical_size();
        let efficiency = if raw_pool_bytes > 0 {
            (usable_pool_bytes as f64 / raw_pool_bytes as f64) * 100.0
        } else {
            0.0
        };
        let total_superblocks = geom.num_superblocks();
        let raw_block_size = BLOCK_SIZE;
        let raw_superblock_size = BLOCK_SIZE;
        let logical_superblock_size = geom.superblock_size();

        let disks_info: Vec<DiskInfo> = disks
            .iter()
            .map(|d| DiskInfo {
                path: PathBuf::from(&d.path),
                size: d.size as u64,
                block_size: d.block_size as u64,
                serial: d.serial.clone(),
            })
            .collect();

        Ok(InfoResult {
            total_disks,
            smallest_disk_raw_bytes,
            raw_pool_bytes,
            usable_pool_bytes,
            efficiency,
            total_superblocks,
            raw_block_size,
            raw_superblock_size,
            logical_superblock_size,
            disks: disks_info,
        })
    }
}

fn register_disks(
    tx: &rusqlite::Transaction,
    disk_paths: &[PathBuf],
) -> Result<u64> {
    let mut disk_sizes = Vec::new();

    for (id, disk_path) in disk_paths.iter().enumerate() {
        let abs_path = fs::semi_canonicalize(disk_path)
            .unwrap_or_else(|_| disk_path.clone());

        let size = fs::forcefully_get_file_size(&abs_path)
            .with_context(|| format!("Failed to get size of disk file {:?}", disk_path))?;
        let path_str = abs_path.to_string_lossy().to_string();

        let serial = sys::get_disk_serial(&abs_path)
            .unwrap_or_else(|_| None);

        let block_size = sys::get_block_size(&abs_path)
            .unwrap_or_else(|_| None);

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
