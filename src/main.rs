mod db;
mod disk;
mod pool;
mod sys;

use std::path::PathBuf;
use clap::{Parser, Subcommand};
use anyhow::{Context, Result};
use pool::PoolGeometry;
use blake3::Hasher;
use std::io::{Read, Seek};

#[derive(Parser)]
#[command(name = "shoddy-raid")]
#[command(about = "A command line tool for managing a shoddy raid pool", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new pool
    Create {
        /// Path to the SQLite database
        db_path: PathBuf,

        /// Paths to disk files or devices
        disk_paths: Vec<PathBuf>,
    },
    /// Display information about a pool
    Info {
        /// Path to the SQLite database
        db_path: PathBuf,
    },
    /// Add a file to the pool
    AddFile {
        /// Path to the SQLite database
        db_path: PathBuf,

        /// Path to the file to add
        file_path: PathBuf,

        /// Name to store the file as in the filesystem
        #[arg(short, long)]
        name: String,
    },
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if bytes >= TB {
        format!("{:.2} TiB ({})", bytes as f64 / TB as f64, insert_commas(bytes))
    } else if bytes >= GB {
        format!("{:.2} GiB ({})", bytes as f64 / GB as f64, insert_commas(bytes))
    } else if bytes >= MB {
        format!("{:.2} MiB ({})", bytes as f64 / MB as f64, insert_commas(bytes))
    } else if bytes >= KB {
        format!("{:.2} KiB ({})", bytes as f64 / KB as f64, insert_commas(bytes))
    } else {
        format!("{} bytes", insert_commas(bytes))
    }
}

fn insert_commas(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let len = s.len();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Create { db_path, disk_paths } => {
            if disk_paths.is_empty() {
                return Err(anyhow::anyhow!("At least 2 disks are required to create a RAID pool."));
            }
            if disk_paths.len() < 2 {
                return Err(anyhow::anyhow!("A RAID pool requires at least 2 disks (including 1 parity disk)."));
            }

            println!("Creating pool database at: {:?}", db_path);
            db::create_pool(&db_path, &disk_paths)
                .context("Failed to create raid pool")?;
            
            println!("Successfully initialized pool with {} disks.", disk_paths.len());
        }
        Commands::AddFile { db_path, file_path, name } => {
            println!("Adding file to pool:");
            println!("  Database:  {:?}", db_path);
            println!("  File:      {:?}", file_path);
            println!("  Name:      {}", name);

            // Split name into path components (handles both / and \)
            let path_components: Vec<&str> = name.split(|c| c == '/' || c == '\\')
                .filter(|s| !s.is_empty())
                .collect();

            // Check if file already exists
            if db::file_exists(&db_path, &path_components)? {
                return Err(anyhow::anyhow!("File '{}' already exists in the pool", name));
            }

            // Get pool geometry from database
            let disks = db::get_disks(&db_path)
                .context("Failed to retrieve pool information")?;
            
            if disks.is_empty() {
                return Err(anyhow::anyhow!("No disks registered in the pool."));
            }

            let num_disks = disks.len();
            let min_disk_size = disks.iter().map(|d| d.size as u64).min().unwrap_or(0);
            let geom = PoolGeometry::new(num_disks, min_disk_size);

            let file_size = std::fs::metadata(&file_path)
                .with_context(|| format!("Failed to read file metadata {:?}", file_path))?
                .len();
            println!("  File size: {}", format_bytes(file_size));

            // Calculate number of superblocks needed
            let logical_block_size = geom.logical_size();
            if logical_block_size == 0 {
                return Err(anyhow::anyhow!("Invalid pool geometry: logical block size is zero"));
            }
            let required_superblocks = (file_size + logical_block_size - 1) / logical_block_size;
            println!("  Superblocks needed: {}", required_superblocks);

            // Allocate superblocks from the pool
            let allocated_ids = db::get_free_superblocks(&db_path, required_superblocks as i64)
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
            let mut file = std::fs::File::open(&file_path)
                .with_context(|| format!("Failed to open file {:?}", file_path))?;

            // Create hasher for whole-file hash
            let mut hasher = Hasher::new();

            // Process each superblock
            for (i, &superblock_id) in allocated_ids.iter().enumerate() {
                let start = (i as u64) * logical_block_size;
                if start >= file_size{
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
                
                disk::write_superblock(superblock_id, superblock_data, &geom, &mut disk_files)?;
            }
        }
        Commands::Info { db_path } => {
            let disks = db::get_disks(&db_path)
                .context("Failed to retrieve pool information")?;
            
            if disks.is_empty() {
                println!("Database exists but no disks are registered in this pool.");
                return Ok(());
            }

            let num_disks = disks.len();
            let min_disk_size = disks.iter().map(|d| d.size as u64).min().unwrap_or(0);
            let geom = PoolGeometry::new(num_disks, min_disk_size);

            let logical_size = geom.logical_size();
            let physical_size = geom.physical_size();
            let efficiency = if physical_size > 0 {
                (logical_size as f64 / physical_size as f64) * 100.0
            } else {
                0.0
            };

            // Check if there is disk size mismatch
            let size_mismatch = disks.iter().any(|d| d.size as u64 != min_disk_size);

            println!("====================================================");
            println!("SHODDY-RAID POOL INFO");
            println!("====================================================");
            println!("Database Path: {:?}", db_path);
            println!("Disk Count:    {} ({} Data, 1 Parity)", num_disks, num_disks - 1);
            if size_mismatch {
                println!("WARNING:       Disks have mismatched sizes! Pool size is constrained");
                println!("               by the smallest disk size: {}", format_bytes(min_disk_size));
            }
            println!("----------------------------------------------------");
            println!("Geometry & Capacity:");
            println!("  Block size:          516 KiB (528,384 bytes)");
            println!("  Total Superblocks:   {}", insert_commas(geom.num_superblocks()));
            println!("  Logical Capacity:    {}", format_bytes(logical_size));
            println!("  Physical Capacity:   {}", format_bytes(physical_size));
            println!("  Space Efficiency:    {:.1}%", efficiency);
            println!("----------------------------------------------------");
            println!("Registered Disks:");
            for disk in disks {
                println!(
                    "  ID {}: {} (Size: {}, Block Size: {} KiB, Serial: {})",
                    disk.id,
                    disk.path,
                    format_bytes(disk.size as u64),
                    disk.block_size / 1024,
                    disk.serial.as_deref().unwrap_or("N/A")
                );
            }
            println!("====================================================");
        }
    }

    Ok(())
}
