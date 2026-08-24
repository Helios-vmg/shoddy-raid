#![allow(dead_code)]

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
