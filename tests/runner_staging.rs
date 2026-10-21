mod common;

use common::{default_host, Decorate, FakeHost, Wrap};
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use truenas_nvidia_driver_manager::host::{Create, Host, SyncWrite};
use truenas_nvidia_driver_manager::{store::Paths, tasks};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    None,
    Open,
    Read,
    Create,
    Write,
    Sync,
    Rename,
    DirectorySync,
}

struct StagingHost {
    host: FakeHost,
    fault: Fault,
}

struct FailingRead(bool);
impl Read for FailingRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.0 {
            return Err(io::Error::other("source read refused"));
        }
        self.0 = true;
        let n = buf.len().min(3);
        buf[..n].copy_from_slice(&b"new"[..n]);
        Ok(n)
    }
}

struct FailingWrite {
    inner: Box<dyn SyncWrite + Send>,
    wrote: bool,
}
impl Write for FailingWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.wrote {
            return Err(io::Error::other("destination write refused"));
        }
        self.wrote = true;
        self.inner.write(&bytes[..bytes.len().min(3)])
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
impl SyncWrite for FailingWrite {
    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }
}

impl Decorate for StagingHost {
    fn inner(&self) -> &dyn Host {
        &self.host
    }
    fn open(&self, p: &Path) -> io::Result<Box<dyn Read + Send>> {
        assert_eq!(p, Path::new("/proc/self/exe"));
        match self.fault {
            Fault::Open => Err(io::Error::other("source open refused")),
            Fault::Read => Ok(Box::new(FailingRead(false))),
            _ => self.host.open(p),
        }
    }
    fn create(&self, p: &Path, how: Create, mode: u32) -> io::Result<Box<dyn SyncWrite + Send>> {
        if self.fault == Fault::Create {
            return Err(io::Error::other("destination create refused"));
        }
        let inner = self.host.create(p, how, mode)?;
        if self.fault == Fault::Write {
            Ok(Box::new(FailingWrite {
                inner,
                wrote: false,
            }))
        } else {
            Ok(inner)
        }
    }
}

fn check(fault: Fault) {
    for had_runner in [false, true] {
        let host = default_host();
        let paths = Paths::new("/var/lib/nvm");
        let runner = paths.runner_bin();
        let runner_path = runner.to_str().unwrap();
        host.set_exe(b"new executable bytes");
        if had_runner {
            let mut file = host.create(&runner, Create::New, 0o755).unwrap();
            file.write_all(b"previous executable bytes").unwrap();
        }
        match fault {
            Fault::Sync => host.fail_sync_file(paths.bin().to_str().unwrap()),
            Fault::Rename => host.fail_rename_to_suffix("truenas-nvidia-driver-manager"),
            Fault::DirectorySync => host.fail_fsync_dir(paths.bin().to_str().unwrap()),
            _ => {}
        }
        let decorated = Wrap(StagingHost {
            host: host.clone(),
            fault,
        });
        assert_eq!(
            tasks::stage_runner(&decorated, &paths).is_ok(),
            fault == Fault::None,
            "{fault:?}"
        );
        let published = matches!(fault, Fault::None | Fault::DirectorySync);
        let expected = if published {
            Some(b"new executable bytes".to_vec())
        } else if had_runner {
            Some(b"previous executable bytes".to_vec())
        } else {
            None
        };
        assert_eq!(
            host.file(runner_path),
            expected,
            "{fault:?}, previous={had_runner}"
        );
        if expected.is_some() {
            assert_eq!(
                host.lstat(&runner).unwrap().permissions().mode() & 0o777,
                0o755,
                "{fault:?}"
            );
        }
        let expected_names = if expected.is_some() {
            vec!["truenas-nvidia-driver-manager".to_string()]
        } else {
            vec![]
        };
        assert_eq!(
            host.children(paths.bin().to_str().unwrap()),
            expected_names,
            "temporary files leaked after {fault:?}"
        );
        host.assert_red_lines();
    }
}

#[test]
fn success() {
    check(Fault::None);
}
#[test]
fn source_open_failure() {
    check(Fault::Open);
}
#[test]
fn partial_source_read_failure() {
    check(Fault::Read);
}
#[test]
fn destination_create_failure() {
    check(Fault::Create);
}
#[test]
fn partial_destination_write_failure() {
    check(Fault::Write);
}
#[test]
fn file_sync_failure() {
    check(Fault::Sync);
}
#[test]
fn rename_failure() {
    check(Fault::Rename);
}
#[test]
fn directory_sync_failure_keeps_the_published_runner() {
    check(Fault::DirectorySync);
}
