use anyhow::{Context, Result};
use crate::pool::{PoolGeometry, DATA_SIZE, SUBBLOCK_SIZE, BLOCK_SIZE};
use std::fs::File;
use std::io::Write;
use std::io::Seek;

/// Writes a superblock to the pool.
/// Pads the data to the logical superblock size by repeating it.
pub fn write_superblock(
    superblock_id: u64,
    mut data: Vec<u8>,
    geom: &PoolGeometry,
    disk_files: &mut [File],
) -> Result<()> {
    // Defensive check: ensure disk_files.len() matches geom.num_disks
    if disk_files.len() != geom.num_disks {
        return Err(anyhow::anyhow!(
            "Disk file count mismatch: expected {}, got {}",
            geom.num_disks,
            disk_files.len()
        ));
    }
    
    // Calculate logical superblock size using PoolGeometry
    let logical_superblock_size = geom.logical_size() as usize;

    if data.len() < logical_superblock_size{
        let copy = data.as_slice()[0..(logical_superblock_size - data.len())]
            .to_vec();
        data.extend_from_slice(&copy);
    }

    // Split data into 4096-byte subblocks
    let subblocks: Vec<Vec<u8>> = data
        .chunks_exact(SUBBLOCK_SIZE as usize)
        .map(|chunk| chunk.to_vec())
        .collect();

    // Further split into (num_disks - 1) groups of 128 subblocks each
    // Horizontal layout: fill each disk completely before moving to the next
    let num_data_disks = geom.num_disks - 1;
    let subblocks_per_disk = 128;
    
    let mut disk_data: Vec<Vec<Vec<u8>>> = vec![vec![]; num_data_disks];
    
    for (i, subblock) in subblocks.into_iter().enumerate() {
        let disk_index = i / subblocks_per_disk;
        disk_data[disk_index].push(subblock);
    }
    
    // Create parity data by XORing all data disks
    let mut parity: Vec<Vec<u8>> = Vec::with_capacity(subblocks_per_disk);
    for i in 0..subblocks_per_disk {
        let mut parity_block = [0u8; SUBBLOCK_SIZE as usize];
        for disk in disk_data.iter() {
            let data = &disk[i];
            for j in 0..SUBBLOCK_SIZE as usize {
                parity_block[j] ^= data[j];
            }
        }
        parity.push(parity_block.to_vec());
    }
    
    // Move parity into the back of disk_data
    disk_data.push(parity);
    
    // Compute BLAKE3 hashes for each subblock and append to each disk
    for disk in disk_data.iter_mut() {
        let mut hash_subblock = vec![0u8; 32 * 128]; // 32 bytes per hash * 128 subblocks
        for (i, subblock) in disk.iter().enumerate() {
            let hash = blake3::hash(subblock);
            hash_subblock[i * 32..(i + 1) * 32].copy_from_slice(hash.as_bytes());
        }
        disk.push(hash_subblock);
    }
    
    // Write each disk's data to its corresponding file
    let block_offset = superblock_id * BLOCK_SIZE;
    
    for (disk_idx, disk_data) in disk_data.iter().enumerate() {
        let file = &mut disk_files[disk_idx];
        
        file.seek(std::io::SeekFrom::Start(block_offset))
            .with_context(|| format!("Failed to seek in disk {}", disk_idx))?;

        for subblock_data in disk_data.iter() {
            file.write_all(subblock_data)
                .with_context(|| format!("Failed to write to disk {}", disk_idx))?;
        }
    }
    
    Ok(())
}
