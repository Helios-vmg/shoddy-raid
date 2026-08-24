use std::path::PathBuf;

pub fn parse_size(size_str: &str) -> Result<u64, String> {
    let size_str = size_str.trim().to_uppercase();
    
    if size_str.is_empty() {
        return Err("Size cannot be empty".to_string());
    }

    let (num_part, unit_part) = size_str.split_at(size_str.find(|c: char| c.is_alphabetic()).unwrap_or(size_str.len()));
    
    let num: u64 = num_part.parse()
        .map_err(|_| format!("Invalid number: '{}'", num_part))?;
    
    let multiplier = match unit_part {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024 * 1024 * 1024 * 1024,
        _ => return Err(format!("Invalid unit: '{}'. Valid units: B, K/KIB/KB, M/MIB/MB, G/GIB/GB, T/TIB/TB", unit_part)),
    };

    Ok(num * multiplier)
}

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if bytes >= TB {
        format!("{:.2} TiB ({})", bytes as f64 / TB as f64, add_thousands_separators(bytes))
    } else if bytes >= GB {
        format!("{:.2} GiB ({})", bytes as f64 / GB as f64, add_thousands_separators(bytes))
    } else if bytes >= MB {
        format!("{:.2} MiB ({})", bytes as f64 / MB as f64, add_thousands_separators(bytes))
    } else if bytes >= KB {
        format!("{:.2} KiB ({})", bytes as f64 / KB as f64, add_thousands_separators(bytes))
    } else {
        format!("{} bytes", add_thousands_separators(bytes))
    }
}

pub fn add_thousands_separators(n: u64) -> String {
    let s = n.to_string();
    let mut result = String::new();
    let len = s.len();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}

#[allow(dead_code)]
pub fn split_path(name: &str) -> Vec<&str> {
    name.split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect()
}

#[allow(dead_code)]
pub fn join_path(components: &[&str]) -> PathBuf {
    components.join("/").into()
}
