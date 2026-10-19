//! Library of the TrueNAS NVIDIA driver manager.
//!
//! The binary has two modes: no argument (or `serve`) runs the web process, `task run <id>`
//! runs the runner. Integration tests use this library to run both in one process.

pub mod admission;
pub mod auth;
pub mod build_kit;
pub mod catalog;
pub mod codes;
pub mod drivers;
pub mod facts;
pub mod gpu_table;
pub mod gpu_users;
pub mod host;
pub mod log;
pub mod plan;
pub mod runner;
pub mod settings;
pub mod ssh_login;
pub mod startup;
pub mod store;
pub mod task_state;
pub mod tasks;
pub mod upload;
pub mod web;

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};

    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("nvm-{label}-{}", crate::store::random_hex(8)));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl std::ops::Deref for TestDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TestDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub mod setup_code;
