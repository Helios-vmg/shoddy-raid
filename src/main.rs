mod db;
mod disk;
mod file_ops;
mod pool;
mod sys;
mod utils;

use std::path::PathBuf;
use clap::{Parser, Subcommand};
use anyhow::{Context, Result};
use pool::PoolGeometry;
use rusqlite::{Connection};

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
        Commands::AddFile { db_path, file_path, name, force } => {
            println!("Adding file to pool:");
            println!("  Database:  {:?}", db_path);
            println!("  File:      {:?}", file_path);
            println!("  Name:      {}", name);

            let dst_path = utils::split_path(&name);

            let mut conn = Connection::open(&db_path)
                .with_context(|| format!("Failed to open database at {db_path:?}"))?;
            let tx = conn.transaction()
                .context("Failed to start database transaction")?;

            let operation = {
                if db::file_exists_with_tx(&tx, &dst_path)? {
                    if !force {
                        return Err(anyhow::anyhow!("File '{}' already exists in the pool", name));
                    }
                    file_ops::replace_file(&tx, &file_path, &dst_path)?;
                    "replaced in"
                }else{
                    file_ops::add_file(&tx, &file_path, &dst_path)?;
                    "added to"
                }
            };

            tx.commit()
                .context("Failed to commit database transaction")?;
            
            println!("  File '{name}' successfully {operation} pool");
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
                println!("               by the smallest disk size: {}", utils::format_bytes(min_disk_size));
            }
            println!("----------------------------------------------------");
            println!("Geometry & Capacity:");
            println!("  Block size:          516 KiB (528,384 bytes)");
            println!("  Total Superblocks:   {}", utils::add_thousands_separators(geom.num_superblocks()));
            println!("  Logical Capacity:    {}", utils::format_bytes(logical_size));
            println!("  Physical Capacity:   {}", utils::format_bytes(physical_size));
            println!("  Space Efficiency:    {:.1}%", efficiency);
            println!("----------------------------------------------------");
            println!("Registered Disks:");
            for disk in disks {
                println!(
                    "  ID {}: {} (Size: {}, Block Size: {} KiB, Serial: {})",
                    disk.id,
                    disk.path,
                    utils::format_bytes(disk.size as u64),
                    disk.block_size / 1024,
                    disk.serial.as_deref().unwrap_or("N/A")
                );
            }
            println!("====================================================");
        }
    }

    Ok(())
}
