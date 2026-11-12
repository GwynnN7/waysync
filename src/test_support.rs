use crate::config::AppConfig;

pub fn fixture() -> (tempfile::TempDir, AppConfig) {
    let root = tempfile::tempdir().unwrap();
    let config = AppConfig {
        state_dir: root.path().join("state"),
        data_dir: root.path().join("data"),
        repo_dir: root.path().join("repo"),
        state_file: root.path().join("state/state.toml"),
    };
    config.ensure_dirs().unwrap();
    std::fs::create_dir_all(config.dotfiles_dir()).unwrap();
    (root, config)
}

pub fn write(path: impl AsRef<std::path::Path>, content: &str) {
    let path = path.as_ref();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}
