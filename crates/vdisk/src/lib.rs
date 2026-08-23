use anyhow::Result;
use std::fs::{
    File,
    OpenOptions,
};
use std::io::{
    Read,
    Write,
    Seek,
    SeekFrom,
};
use std::path::PathBuf;

const MAGIC: [u8; 4] = *b"GNAF";
const VERSION: u32 = 1;
const BLOCK_SIZE: usize = 1 << 20; // 1 MiB
const BLOCK_SIZE64: u64 = BLOCK_SIZE as u64;
const HEADER_SIZE: usize = 4096;
const ALLOCATED_BLOCKS_OFFSET: usize = 16;

/// A virtual disk that stores data in a file
pub struct VDisk {
    file: File,
    block_count: u64,
    allocated_block_count: u64,
    block_pointers: Vec<u64>,
    uncommitted_blocks: Vec<(u64, u64)>,
}

impl VDisk {
    /// Opens an existing virtual disk file
    pub fn open(path: PathBuf) -> Result<Self> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?;

        // Read header
        let mut header = [0u8; 4096];
        file.read_exact(&mut header)?;

        // Validate magic number and version
        let magic = &header[0..4];
        if magic != MAGIC {
            return Err(anyhow::anyhow!("Invalid magic number"));
        }

        let version = u32::from_le_bytes(header[4..8].try_into().unwrap());
        if version != VERSION {
            return Err(anyhow::anyhow!("Unsupported version"));
        }

        let block_count = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let allocated_block_count = u64::from_le_bytes(header[ALLOCATED_BLOCKS_OFFSET..][..size_of::<u64>()].try_into().unwrap());

        // Read block table
        let block_table_size = block_count as usize * 8;
        let mut block_table_bytes = vec![0u8; block_table_size];
        file.read_exact(&mut block_table_bytes)?;

        let mut block_pointers = Vec::with_capacity(block_count as usize);
        for i in 0..block_count as usize {
            let offset = i * 8;
            let bytes: [u8; 8] = block_table_bytes[offset..offset + 8].try_into().unwrap();
            block_pointers.push(u64::from_le_bytes(bytes));
        }

        Ok(Self {
            file,
            block_count,
            allocated_block_count,
            block_pointers,
            uncommitted_blocks: Vec::new(),
        })
    }

    /// Creates a new virtual disk file with the specified size
    pub fn create(path: PathBuf, size: u64) -> Result<Self> {
        // Check if file already exists
        if path.exists() {
            return Err(anyhow::anyhow!("File already exists"));
        }

        // Calculate block counts
        let block_count = (size + BLOCK_SIZE64 - 1) / BLOCK_SIZE64;

        // Calculate file size: header (4K) + block_table + blocks
        let header_size: u64 = 4096;
        let block_table_size = Self::block_table_size(block_count);
        let file_size = header_size + block_table_size;

        // Create and truncate the file
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;
        file.set_len(file_size)?;

        // Write header
        let mut header = [0u8; 4096];
        header[0..4].copy_from_slice(&MAGIC);
        header[4..8].copy_from_slice(&VERSION.to_le_bytes());
        header[8..16].copy_from_slice(&block_count.to_le_bytes());
        //allocated_blocks = 0
        header[ALLOCATED_BLOCKS_OFFSET..ALLOCATED_BLOCKS_OFFSET + size_of::<u64>()]
        .copy_from_slice(&[0u8; size_of::<u64>()]);

        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header)?;

        // Initialize block table to zeros (already done by set_len, but explicit)
        // The block table starts at offset 4096 and is block_count * 8 bytes

        Ok(Self {
            file,
            block_count,
            allocated_block_count: 0,
            block_pointers: vec![0; block_count as usize],
            uncommitted_blocks: Vec::new(),
        })
    }

    fn block_table_size(blocks: u64) -> u64{
        blocks * size_of::<u64>() as u64
    }

    /// Reads a block into the provided buffer
    pub fn read_block_with_offset(&mut self, block_index: u64, offset: usize, mut dst: &mut [u8]) -> Result<usize> {
        // Validate block index
        if block_index >= self.block_count {
            return Err(anyhow::anyhow!("Block index out of range"));
        }

        // Validate offset
        if offset >= BLOCK_SIZE {
            return Err(anyhow::anyhow!("Offset out of range"));
        }

        if offset + dst.len() > BLOCK_SIZE {
            dst = &mut dst[..BLOCK_SIZE - offset];
        }

        // Get the block pointer from the block table
        let block_pointer = self.block_pointers[block_index as usize];

        if block_pointer == 0 {
            // Block is unallocated.
            dst.fill(0);
        }else{
            // Seek to the offset and read.
            self.file.seek(SeekFrom::Start(block_pointer + offset as u64))?;
            self.file.read_exact(dst)?;
        }

        Ok(dst.len())
    }

    /// Reads data from the virtual disk into the provided buffer
    pub fn read(&mut self, offset: u64, dst: &mut [u8]) -> Result<usize> {
        if dst.is_empty() {
            return Ok(0);
        }

        // Calculate total disk size
        let disk_size = self.block_count * BLOCK_SIZE64;

        // Validate offset
        if offset > disk_size {
            return Err(anyhow::anyhow!("Offset out of range"));
        }

        // Validate buffer doesn't exceed disk size
        if offset + dst.len() as u64 > disk_size {
            return Err(anyhow::anyhow!("Read exceeds disk size"));
        }

        let mut dst = dst;
        let mut bytes_read = 0;
        let mut offset = offset;
        while dst.len() > 0 {
            let block_index = offset / BLOCK_SIZE64;
            let block_offset = (offset % BLOCK_SIZE64) as usize;

            let read = self.read_block_with_offset(block_index, block_offset, dst)?;

            dst = &mut dst[read..];
            bytes_read += read;
            offset += read as u64;
        }

        Ok(bytes_read)
    }

    fn write_zeroes(&mut self, mut length: usize) -> Result<()> {
        let zero = &[0u8; 1 << 14];
        while length > 0{
            let n = length.min(zero.len());
            self.file.write_all(&zero[0..n])?;
            length -= n;
        }
        Ok(())
    }

    pub fn file_size(&self) -> u64 {
        HEADER_SIZE as u64 + Self::block_table_size(self.block_count) + self.allocated_block_count * BLOCK_SIZE64
    }

    /// Writes a block with offset, allocating if necessary
    pub fn write_block_with_offset(&mut self, block_index: u64, offset: usize, mut buffer: &[u8]) -> Result<usize> {
        // Validate block index
        if block_index >= self.block_count {
            return Err(anyhow::anyhow!("Block index out of range"));
        }

        // Validate offset
        if offset >= BLOCK_SIZE {
            return Err(anyhow::anyhow!("Offset out of range"));
        }

        if offset + buffer.len() > BLOCK_SIZE {
            buffer = &buffer[..BLOCK_SIZE - offset];
        }

        if buffer.is_empty(){
            return Ok(0);
        }

        // Get the block pointer from the block table
        let block_pointer = self.block_pointers[block_index as usize];

        let offset = offset as usize;

        if block_pointer != 0 {
            // Block is already allocated - seek to offset and write
            self.file.seek(SeekFrom::Start(block_pointer + offset as u64))?;

            // Write the data
            self.file.write_all(buffer)?;

            // Write padding if needed
            //let written = offset + buffer.len();
            //self.write_zeroes(BLOCK_SIZE - written)?;
        } else {
            // Block is unallocated - need to allocate it
            // Get current file size
            let current_len = self.file_size();

            // Update block pointer
            self.block_pointers[block_index as usize] = current_len;
            self.uncommitted_blocks.push((block_index, current_len));
            self.allocated_block_count += 1;

            self.file.seek(SeekFrom::Start(current_len))?;
            self.write_zeroes(offset)?;
            self.file.write_all(buffer)?;
            self.write_zeroes(BLOCK_SIZE - (offset + buffer.len()))?;
        }

        Ok(buffer.len())
    }

    /// Commits changes to the file header and block table
    pub fn commit_changes(&mut self) -> Result<()> {
        if self.uncommitted_blocks.is_empty() {
            return Ok(());
        }
        self.file.seek(SeekFrom::Start(ALLOCATED_BLOCKS_OFFSET as u64))?;
        self.file.write_all(&self.allocated_block_count.to_le_bytes())?;

        self.uncommitted_blocks.sort_by_key(|&(block_index, _)| block_index);

        // Update block table with uncommitted blocks
        for (block_index, block_pointer) in &self.uncommitted_blocks {
            let block_table_offset = HEADER_SIZE + (*block_index as usize * size_of::<u64>());
            self.file.seek(SeekFrom::Start(block_table_offset as u64))?;
            self.file.write_all(&block_pointer.to_le_bytes())?;
        }

        // Clear uncommitted blocks
        self.uncommitted_blocks.clear();

        Ok(())
    }

    /// Writes data to the virtual disk from the provided buffer
    pub fn write(&mut self, offset: u64, src: &[u8]) -> Result<usize> {
        if src.is_empty() {
            return Ok(0);
        }

        // Calculate total disk size
        let disk_size = self.block_count * BLOCK_SIZE64;

        // Validate offset
        if offset > disk_size {
            return Err(anyhow::anyhow!("Offset out of range"));
        }

        // Validate buffer doesn't exceed disk size
        if offset + src.len() as u64 > disk_size {
            return Err(anyhow::anyhow!("Write exceeds disk size"));
        }

        let mut src = src;
        let mut bytes_written = 0;
        let mut offset = offset;

        while src.len() > 0 {
            let block_index = offset / BLOCK_SIZE64;
            let block_offset = (offset % BLOCK_SIZE64) as usize;

            let written = self.write_block_with_offset(block_index, block_offset, src)?;

            src = &src[written..];
            bytes_written += written;
            offset += written as u64;
        }

        // Commit changes to the file
        self.commit_changes()?;

        Ok(bytes_written)
    }
}

/// A stream wrapper around VDisk that tracks position
pub struct VDiskStream<'a> {
    vdisk: &'a mut VDisk,
    position: u64,
}

impl<'a> VDiskStream<'a> {
    /// Creates a new VDiskStream
    pub fn new(vdisk: &'a mut VDisk) -> Self {
        Self { vdisk, position: 0 }
    }

    /// Returns the current position in the stream
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Seeks to a new position in the stream
    pub fn seek(&mut self, pos: u64) -> Result<()> {
        let disk_size = self.vdisk.block_count * BLOCK_SIZE64;
        if pos > disk_size {
            return Err(anyhow::anyhow!("Seek position out of range"));
        }
        self.position = pos;
        Ok(())
    }

    /// Seeks relative to current position
    pub fn seek_relative(&mut self, offset: i64) -> Result<u64> {
        let new_pos = if offset >= 0 {
            self.position.saturating_add(offset as u64)
        } else {
            self.position.saturating_sub((-offset) as u64)
        };
        self.seek(new_pos)?;
        Ok(new_pos)
    }
}

impl<'a> Read for VDiskStream<'a> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.vdisk.block_count * BLOCK_SIZE64 {
            return Ok(0);
        }

        let remaining = (self.vdisk.block_count * BLOCK_SIZE64 - self.position) as usize;
        let to_read = buf.len().min(remaining);

        match self.vdisk.read(self.position, &mut buf[..to_read]) {
            Ok(bytes_read) => {
                self.position += bytes_read as u64;
                Ok(bytes_read)
            }
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to read from VDisk: {:?}", e)))
        }
    }
}

impl<'a> Write for VDiskStream<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.position >= self.vdisk.block_count * BLOCK_SIZE64 && !buf.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "Write beyond disk size"));
        }

        let remaining = (self.vdisk.block_count * BLOCK_SIZE64 - self.position) as usize;
        let to_write = buf.len().min(remaining);

        match self.vdisk.write(self.position, &buf[..to_write]) {
            Ok(bytes_written) => {
                self.position += bytes_written as u64;
                Ok(bytes_written)
            }
            Err(e) => Err(std::io::Error::new(std::io::ErrorKind::Other, format!("Failed to write to VDisk: {:?}", e))),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> Seek for VDiskStream<'a> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::Start(offset) => {
                self.seek(offset)
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek failed"))?;
                Ok(offset)
            },
            SeekFrom::Current(offset) => {
                self.seek_relative(offset)
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek failed"))
            },
            SeekFrom::End(offset) => {
                let disk_size = self.vdisk.block_count * BLOCK_SIZE64;
                if offset > 0 || -offset as u64 > disk_size {
                    Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek failed"))
                }else{
                    let ret = disk_size + -offset as u64;
                    self.seek(ret)
                        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek failed"))?;
                    Ok(ret)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{Rng, RngCore, SeedableRng};
    use sha2::{Sha256, Digest};
    use std::fs::File;
    use std::io::{Read, Write, Seek, SeekFrom};
    use tempfile::TempDir;

    #[test]
    fn test_vdisk_vs_file_sha256() {
        // Create temporary directory for test files
        let mut temp_dir = TempDir::new().expect("Failed to create temp directory");
        let vdisk_path = temp_dir.path().join("test.vdisk");
        let file_path = temp_dir.path().join("test.dat");

        // 1. Initialize a real 1 GiB file
        let disk_size: u64 = 1 << 30; // 1 GiB
        
        // 2. Initialize a 1 GiB VDisk
        let mut vdisk = VDisk::create(vdisk_path, disk_size).expect("Failed to create VDisk");

        // 3. 100 times, generate random data and write to both
        let mut rng = rand::rngs::StdRng::seed_from_u64(42); // Fixed seed for reproducibility

        {
            let mut real_file = File::create(&file_path).expect("Failed to create real file");
            real_file.set_len(disk_size).expect("Failed to set file size");
            for _ in 0..100 {
                // Generate 2^uniform_random(6, 24) random bytes
                let exp = rng.r#gen::<f64>() * (26.0 - 6.0) + 6.0;
                let buffer_size = 2f64.powf(exp).round() as usize;
                // Generate random position
                let max_position = disk_size.saturating_sub(buffer_size as u64);
                let position: u64 = rng.gen_range(0..=max_position);
                println!("Writing {buffer_size} bytes at position {position}");

                // Generate random data
                let mut buffer = vec![0u8; buffer_size];
                let _ = rng.try_fill_bytes(&mut buffer);

                // Write to real file
                real_file.seek(SeekFrom::Start(position)).expect("Failed to seek real file");
                real_file.write_all(&buffer).expect("Failed to write to real file");

                // Write to VDisk
                vdisk.write(position, &buffer).expect("Failed to write to VDisk");
            }
        }

        // 4. Get linear SHA-256 of both
        // SHA-256 of real file
        let real_hash = {
            let mut real_file = File::open(&file_path).expect("Failed to open real file for hashing");
            let mut real_hasher = Sha256::new();
            let mut real_buffer = [0u8; 65536]; // 64 KiB buffer
            loop {
                let bytes_read = real_file.read(&mut real_buffer).expect("Failed to read real file");
                if bytes_read == 0 {
                    break;
                }
                real_hasher.update(&real_buffer[..bytes_read]);
            }
            real_hasher.finalize()
        };

        let vdisk_hash = {
            // SHA-256 of VDisk using VDiskStream
            let mut vdisk_stream = VDiskStream::new(&mut vdisk);
            let mut vdisk_hasher = Sha256::new();
            let mut vdisk_buffer = [0u8; 65536];
            loop {
                let bytes_read = vdisk_stream.read(&mut vdisk_buffer).expect("Failed to read VDisk");
                if bytes_read == 0 {
                    break;
                }
                vdisk_hasher.update(&vdisk_buffer[..bytes_read]);
            }
            vdisk_hasher.finalize()
        };

        // 5. The test passes if the hashes match
        assert_eq!(real_hash.as_slice(), vdisk_hash.as_slice(), "SHA-256 hashes do not match");
        /*if real_hash.as_slice() != vdisk_hash.as_slice() {
            // On failure, copy the vdisk as a stream to a new file
            temp_dir.disable_cleanup(true);
            let failure_path = temp_dir.path().join("vdisk_failure_copy.dat");
            let mut failure_file = File::create(&failure_path).expect("Failed to create failure copy file");
            let mut vdisk_stream = VDiskStream::new(&mut vdisk);
            let mut buffer = [0u8; 65536];
            loop {
                let bytes_read = vdisk_stream.read(&mut buffer).expect("Failed to read VDisk for failure copy");
                if bytes_read == 0 {
                    break;
                }
                failure_file.write_all(&buffer[..bytes_read]).expect("Failed to write failure copy");
            }
            eprintln!("Failure copy saved to: {:?}", failure_path);
            
            panic!("SHA-256 hashes do not match. Failure copy saved to {:?}", failure_path);
        }*/

        // 6. Delete the test files (handled by TempDir drop)
    }
}
