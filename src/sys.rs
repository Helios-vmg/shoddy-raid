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

    let descriptor = unsafe { &*(buffer.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
    let offset = descriptor.SerialNumberOffset;

    let mut serial = None;
    if offset != 0 && offset != 0xFFFFFFFF && (offset as usize) < bytes_returned as usize {
        let mut serial_bytes = Vec::new();
        let mut idx = offset as usize;
        while idx < bytes_returned as usize && buffer[idx] != 0 {
            serial_bytes.push(buffer[idx]);
            idx += 1;
        }
        if let Ok(serial_str) = String::from_utf8(serial_bytes) {
            let trimmed = serial_str.trim().to_string();
            if !trimmed.is_empty() {
                serial = Some(trimmed);
            }
        }
    }

    unsafe { CloseHandle(handle) };
    Ok(serial)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn get_disk_serial(_path: &Path) -> Result<Option<String>> {
    Ok(None)
}
