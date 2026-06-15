use std::path::PathBuf;

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
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result
}

pub fn split_path(name: &str) -> Vec<&str> {
    name.split(|c| c == '/' || c == '\\')
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn join_path(components: &[&str]) -> PathBuf {
    components.join("/").into()
}
