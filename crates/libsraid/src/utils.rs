use std::path::PathBuf;

pub fn join_path(components: &[&str]) -> PathBuf {
    components.join("/").into()
}
