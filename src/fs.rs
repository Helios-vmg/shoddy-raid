use anyhow::{
    Result, Context
};
use std::path::{
    Path,
    PathBuf,
};
use std::fs::{
    File,
};
use std::io::{
    Seek,
    SeekFrom,
};

pub fn absolutize(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

/// Converts a relative path to an absolute path by canonicalizing the parent directory
/// and reattaching the original path components.
/// 
/// Given `<whatever>/foo`, returns `fs::canonicalize(<whatever>)/foo`
pub fn semi_canonicalize(path: &Path) -> Result<PathBuf> {
    let path = absolutize(path)?;

    // Split the path into parent and file components
    let parent = path.parent().unwrap_or(Path::new(""));
    let filename = path.file_name().unwrap_or_default();

    // Canonicalize the parent directory
    let parent = std::fs::canonicalize(parent)?;

    // Reconstruct the absolute path
    let mut result = parent;
    if !filename.is_empty() {
        result.push(filename);
    }

    Ok(result)
}

pub fn forcefully_get_file_size(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open file {:?}", path))?;

    file.seek(SeekFrom::End(0))
        .with_context(|| format!("Failed to seek to end of file {:?}", path))?;

    file.stream_position()
        .with_context(|| format!("Failed to get position in file {:?}", path))
}
