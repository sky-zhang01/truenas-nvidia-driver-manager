mod common;

use common::{default_host, Decorate, FakeHost, Wrap};
use std::ffi::OsString;
use std::io;
use std::path::Path;
use truenas_nvidia_driver_manager::codes::*;
use truenas_nvidia_driver_manager::facts::{self, Reading};
use truenas_nvidia_driver_manager::gpu_users;
use truenas_nvidia_driver_manager::host::Host;

struct Unreadable {
    host: FakeHost,
    kind: io::ErrorKind,
}
impl Decorate for Unreadable {
    fn inner(&self) -> &dyn Host {
        &self.host
    }
    fn read(&self, _: &Path) -> io::Result<Vec<u8>> {
        Err(self.kind.into())
    }
    fn read_dir(&self, _: &Path) -> io::Result<Vec<OsString>> {
        Err(self.kind.into())
    }
}

#[test]
fn shared_readers_preserve_all_io_error_keys() {
    for (kind, expected) in [
        (io::ErrorKind::NotFound, REASON_FILE_MISSING),
        (io::ErrorKind::PermissionDenied, REASON_PERMISSION_DENIED),
        (io::ErrorKind::TimedOut, REASON_IO_ERROR),
        (io::ErrorKind::Other, REASON_IO_ERROR),
    ] {
        let host = Wrap(Unreadable {
            host: default_host(),
            kind,
        });
        assert_eq!(facts::read_modules(&host), Reading::Unknown(expected));
        assert_eq!(facts::proc_modules_now(&host), Reading::Unknown(expected));
        assert_eq!(gpu_users::nvidia_dri_nodes(&host), Err(expected));
        let expected_gpu = if kind == io::ErrorKind::TimedOut {
            REASON_TIMED_OUT
        } else {
            expected
        };
        assert_eq!(facts::read_gpus(&host).unwrap_err().reason, expected_gpu);
    }
}

#[test]
fn module_readers_keep_their_different_utf8_contracts() {
    let host = default_host();
    host.put_file("/proc/modules", b"nvidia\xff 1 0 - Live 0x0\n".to_vec());
    let modules = facts::read_modules(&host).value().cloned().unwrap();
    assert_eq!(modules[0].name, "nvidia\u{fffd}");
    assert_eq!(
        facts::proc_modules_now(&host),
        Reading::Unknown(REASON_FORMAT_UNRECOGNIZED)
    );
    host.put_text("/proc/modules", "broken\n");
    assert_eq!(
        facts::read_modules(&host),
        Reading::Unknown(REASON_FORMAT_UNRECOGNIZED)
    );
}
