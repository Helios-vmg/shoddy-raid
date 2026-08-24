mod utils;

use std::path::PathBuf;
use clap::{Parser, Subcommand};
use anyhow::{Context, Result};
use libsraid::LibSRaid;

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

        /// Overwrite existing file
        #[arg(long)]
        force: bool,
    },
    /// Add a directory to the pool
    AddDirectory {
        /// Path to the SQLite database
        db_path: PathBuf,

        /// Path to the directory to add
        dir_path: PathBuf,

        /// Name to store the directory as in the filesystem
        #[arg(short, long)]
        name: String,

        /// Overwrite existing files
        #[arg(long)]
        force: bool,
    },
    /// Scrub the pool and verify data integrity
    Scrub {
        /// Path to the SQLite database
        db_path: PathBuf,
    },
    /// Create a new virtual disk file
    CreateVdisk {
        /// Path to the vdisk file to create
        vdisk_path: PathBuf,

        /// Size of the vdisk (e.g., "1G", "500M")
        #[arg(short, long)]
        size: String,
    },
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
            LibSRaid::create(&db_path, &disk_paths)
                .context("Failed to create raid pool")?;
            
            println!("Successfully initialized pool with {} disks.", disk_paths.len());
        }
        Commands::AddFile { db_path, file_path, name, force } => {
            println!("Adding file to pool:");
            println!("  Database:  {:?}", db_path);
            println!("  File:      {:?}", file_path);
            println!("  Name:      {}", name);

            let dst_path = PathBuf::from(&name);

            let mut raid = LibSRaid::open(&db_path)
                .with_context(|| format!("Failed to open pool at {db_path:?}"))?;
            let result = raid.add_file(&file_path, &dst_path, force)
                .context("Failed to add file to pool")?;
            
            let result = if !result { "added to" } else { "replaced in" };

            println!("  File '{}' successfully {} pool", dst_path.display(), result);
        }
        Commands::AddDirectory { db_path, dir_path, name, force } => {
            println!("Adding directory to pool:");
            println!("  Database:  {:?}", db_path);
            println!("  Directory: {:?}", dir_path);
            println!("  Name:      {}", name);

            let dst_path = PathBuf::from(&name);

            let mut raid = LibSRaid::open(&db_path)
                .with_context(|| format!("Failed to open pool at {db_path:?}"))?;
            raid.add_directory(&dir_path, &dst_path, force)
                .context("Failed to add directory to pool")?;
        }
        Commands::Scrub { db_path } => {
            let mut raid = LibSRaid::open(&db_path)
                .with_context(|| format!("Failed to open pool at {db_path:?}"))?;
            let result = raid.scrub(false)
                .context("Failed to scrub pool")?;
            
            println!("Scrub completed:");
            println!("  Repairable or repaired files: {}", result.repairable_or_repaired_files);
            println!("  Repairable or repaired blocks: {}", result.repairable_or_repaired_blocks);
            println!("  Damaged files: {}", result.damaged_files.len());
            println!("  Raw bytes read: {}", utils::format_bytes(result.raw_bytes_read));
            println!("  Elapsed seconds: {:.3}", result.elapsed_seconds);
        }
        Commands::CreateVdisk { vdisk_path, size } => {
            // Parse the size string (e.g., "1G", "500M")
            let size_bytes = utils::parse_size(&size)
                .map_err(|e| anyhow::anyhow!("Invalid size format: {}", e))?;
            
            println!("Creating vdisk:");
            println!("  Path: {}", vdisk_path.display());
            println!("  Size: {}", utils::format_bytes(size_bytes));

            vdisk::VDisk::create(&vdisk_path, size_bytes)
                .context("Failed to create vdisk")?;
            
            println!("  Vdisk successfully created");
        }
        Commands::Info { db_path } => {
            let mut raid = LibSRaid::open(&db_path)
                .with_context(|| format!("Failed to open pool at {db_path:?}"))?;
            let info = raid.info()
                .context("Failed to retrieve pool info")?;

            // Check if there is disk size mismatch
            let size_mismatch = info.disks.iter().any(|d| d.size != info.smallest_disk_raw_bytes);

            println!("====================================================");
            println!("SHODDY-RAID POOL INFO");
            println!("====================================================");
            println!("Database Path: {:?}", db_path);
            println!("Disk Count:    {} ({} Data, 1 Parity)", info.total_disks, info.total_disks - 1);
            if size_mismatch {
                println!("WARNING:       Disks have mismatched sizes! Pool size is constrained");
                println!("               by the smallest disk size: {}", utils::format_bytes(info.smallest_disk_raw_bytes));
            }
            println!("----------------------------------------------------");
            println!("Geometry & Capacity:");
            println!("  Block size:          {} KiB ({})", info.raw_block_size / 1024, info.raw_block_size);
            println!("  Total Superblocks:   {}", utils::add_thousands_separators(info.total_superblocks));
            println!("  Logical Capacity:    {}", utils::format_bytes(info.usable_pool_bytes));
            println!("  Physical Capacity:   {}", utils::format_bytes(info.raw_pool_bytes));
            println!("  Space Efficiency:    {:.1}%", info.efficiency);
            println!("----------------------------------------------------");
            println!("Registered Disks:");
            for disk in &info.disks {
                println!(
                    "  Path: {} (Size: {}, Block Size: {} KiB, Serial: {})",
                    disk.path.display(),
                    utils::format_bytes(disk.size),
                    disk.block_size / 1024,
                    disk.serial.as_deref().unwrap_or("N/A")
                );
            }
            println!("====================================================");
        }
    }

    Ok(())
}
