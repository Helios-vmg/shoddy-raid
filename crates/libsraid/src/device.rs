use crate::sys;
use anyhow::Result;
use std::io::{Read, Seek, Write};
use std::path::Path;

pub trait Device {
    fn size(&self) -> Result<u64>;
    fn block_size(&self) -> Result<u64>;
    fn serial(&self) -> Result<Option<String>>;
    fn device_type(&self) -> &str;
    fn write(&mut self, offset: u64, data: &[u8]) -> Result<()>;
    fn read(&mut self, offset: u64, dst: &mut [u8]) -> Result<()>;
}

pub fn open_device(path: &Path) -> Result<Box<dyn Device>> {
    Ok(if vdisk::VDisk::is_vdisk(path)? {
        Box::new(vdisk::VDisk::open(path)?)
    } else {
        Box::new(BlockDevice::open(path)?)
    })
}

pub struct BlockDevice {
    file: std::fs::File,
    size: u64,
    block_size: u64,
    serial: Option<String>,
}

impl BlockDevice {
    pub fn open(path: &Path) -> Result<Self> {
        let block_size = sys::get_block_size(path)?.unwrap_or(4096);
        let serial = sys::get_disk_serial(path)?;

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;

        let size = file.seek(std::io::SeekFrom::End(0))?;
        file.seek(std::io::SeekFrom::Start(0))?;

        Ok(Self {
            file,
            size,
            block_size,
            serial,
        })
    }
}

impl Device for BlockDevice {
    fn size(&self) -> Result<u64> {
        Ok(self.size)
    }
    fn block_size(&self) -> Result<u64> {
        Ok(self.block_size)
    }
    fn serial(&self) -> Result<Option<String>> {
        Ok(self.serial.clone())
    }
    fn device_type(&self) -> &str {
        "block"
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        self.file.seek(std::io::SeekFrom::Start(offset))?;
        self.file.write_all(data)?;
        Ok(())
    }

    fn read(&mut self, offset: u64, dst: &mut [u8]) -> Result<()> {
        self.file.seek(std::io::SeekFrom::Start(offset))?;
        self.file.read_exact(dst)?;
        Ok(())
    }
}

impl Device for vdisk::VDisk {
    fn size(&self) -> Result<u64> {
        Ok(self.size())
    }
    fn block_size(&self) -> Result<u64> {
        Ok(4096)
    }
    fn serial(&self) -> Result<Option<String>> {
        Ok(Some(self.serial()))
    }
    fn device_type(&self) -> &str {
        "vdisk"
    }

    fn write(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        self.write(offset, data)?;
        Ok(())
    }

    fn read(&mut self, offset: u64, dst: &mut [u8]) -> Result<()> {
        self.read(offset, dst)?;
        Ok(())
    }
}
