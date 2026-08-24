use std::path::Path;
use anyhow::Result;

#[cfg(target_os = "linux")]
pub fn get_disk_serial(path: &Path) -> Result<Option<String>> {
    use std::fs;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    use udev::{Device, DeviceType};

    let metadata = fs::metadata(path)?;
    let file_type = metadata.file_type();
    
    let dev_type = if file_type.is_block_device() {
        DeviceType::Block
    } else if file_type.is_char_device() {
        DeviceType::Character
    } else {
        // For test files or regular files, there is no udev hardware serial
        return Ok(None);
    };

    let devnum = metadata.rdev();
    if devnum == 0 {
        return Ok(None);
    }

    let device = Device::from_devnum(dev_type, devnum)?;
    let serial = device.property_value("ID_SERIAL_SHORT")
        .map(|val| val.to_string_lossy().into_owned());

    Ok(serial)
}

#[cfg(target_os = "linux")]
pub fn get_block_size(path: &Path) -> Result<Option<u64>> {
    use std::fs;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    use udev::{Device, DeviceType};

    let metadata = fs::metadata(path)?;
    let file_type = metadata.file_type();
    
    let dev_type = if file_type.is_block_device() {
        DeviceType::Block
    } else if file_type.is_char_device() {
        DeviceType::Character
    } else {
        return Ok(None);
    };

    let devnum = metadata.rdev();
    if devnum == 0 {
        return Ok(None);
    }

    let device = Device::from_devnum(dev_type, devnum)?;
    let sector_size = device.property_value("ID_SECTOR_SIZE")
        .and_then(|val| val.to_string_lossy().into_owned().parse::<u64>().ok());

    Ok(sector_size)
}

#[cfg(target_os = "windows")]
pub fn get_disk_serial(path: &Path) -> Result<Option<String>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        StorageDeviceProperty, PropertyStandardQuery,
    };

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Ok(None);
    }

    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0; 1],
    };

    let mut bytes_returned = 0;
    let mut buffer = vec![0u8; 1024];

    let success = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            &query as *const _ as *const _,
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            buffer.as_mut_ptr() as *mut _,
            buffer.len() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if success == 0 {
        unsafe { CloseHandle(handle) };
        return Ok(None);
    }

    let descriptor = unsafe { &*(buffer.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
    let required_size = descriptor.Size as usize;
    if required_size > buffer.len() {
        buffer.resize(required_size, 0);
        let success = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                &query as *const _ as *const _,
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                buffer.as_mut_ptr() as *mut _,
                buffer.len() as u32,
                &mut bytes_returned,
                std::ptr::null_mut(),
            )
        };

        if success == 0 {
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
    }

    let serial_offset = descriptor.SerialNumberOffset as usize;
    let serial_bytes = &buffer[serial_offset..];
    let serial = std::ffi::CStr::from_bytes_with_nul(serial_bytes)
        .ok()
        .and_then(|s| s.to_str().ok())
        .map(|s| s.trim().to_string());

    unsafe { CloseHandle(handle) };
    Ok(serial)
}

#[cfg(target_os = "windows")]
pub fn get_block_size(path: &Path) -> Result<Option<u64>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, DISK_GEOMETRY_EX, STORAGE_DEVICE_NUMBER, IOCTL_STORAGE_GET_DEVICE_NUMBER,
    };

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();

    let handle = unsafe {
        CreateFileW(
            path_wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Ok(None);
    }

    // Try to get disk geometry first
    let mut geometry: DISK_GEOMETRY_EX = unsafe { std::mem::zeroed() };
    let mut bytes_returned = 0;

    let success = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
            std::ptr::null(),
            0,
            &mut geometry as *mut _ as *mut _,
            std::mem::size_of::<DISK_GEOMETRY_EX>() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    if success != 0 {
        let sector_size = geometry.Geometry.BytesPerSector as u64;
        unsafe { CloseHandle(handle) };
        return Ok(Some(sector_size));
    }

    // Fall back to storage device number
    let mut device_number: STORAGE_DEVICE_NUMBER = unsafe { std::mem::zeroed() };
    let success = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            std::ptr::null(),
            0,
            &mut device_number as *mut _ as *mut _,
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            &mut bytes_returned,
            std::ptr::null_mut(),
        )
    };

    unsafe { CloseHandle(handle) };

    if success != 0 {
        // We got the device number, but we need sector size
        // For now, return None if we can't get it from geometry
        Ok(None)
    } else {
        Ok(None)
    }
}
