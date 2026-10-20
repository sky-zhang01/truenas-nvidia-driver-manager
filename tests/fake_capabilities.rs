//! The FakeHost capability classes themselves. These tests exercise the test double, not the
//! product: the real sandbox file tree, the two mount tables, the dataset property, the module and
//! sysext model, the PCI devices, the driver readings, command replies and timeouts, the unit
//! model, the clock and the red-line assertion. Product behaviour is asserted in the other
//! integration files.

mod common;

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use common::fake_host::{FakeHost, FakeOutput, CANONICAL_PATH_FOR_TESTS};
use common::*;
use truenas_nvidia_driver_manager::host::{At, Cmd, Create, Exit, Host, HOST_PATH};

const SHORT: std::time::Duration = std::time::Duration::from_secs(5);

fn cmd(program: &'static str) -> Cmd {
    Cmd::new(At::Host, program, SHORT)
}

// 1. Real file tree ----------------------------------------------------------

#[test]
fn the_file_tree_is_a_real_sandbox() {
    let host = FakeHost::new();
    let real = host.sandbox_root().join("a/b.txt");

    {
        let mut f = host
            .create(Path::new("/a/b.txt"), Create::New, 0o600)
            .unwrap();
        f.write_all(b"hello").unwrap();
        f.sync_all().unwrap();
    }
    assert_eq!(std::fs::read(&real).unwrap(), b"hello");
    assert_eq!(host.read(Path::new("/a/b.txt")).unwrap(), b"hello");

    std::fs::write(&real, b"other").unwrap();
    assert_eq!(host.read(Path::new("/a/b.txt")).unwrap(), b"other");

    // Create/New refuses an existing name, truncate replaces the bytes.
    assert_eq!(
        host.create(Path::new("/a/b.txt"), Create::New, 0o600)
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    {
        let mut f = host
            .create(Path::new("/a/b.txt"), Create::Truncate, 0o600)
            .unwrap();
        f.write_all(b"x").unwrap();
        f.sync_all().unwrap();
    }
    assert_eq!(host.read(Path::new("/a/b.txt")).unwrap(), b"x");

    {
        let mut f = host.append(Path::new("/a/b.txt"), 0o600).unwrap();
        f.write_all(b"y").unwrap();
    }
    assert_eq!(host.read(Path::new("/a/b.txt")).unwrap(), b"xy");

    for kind in ["create_new", "create_truncate", "read", "append"] {
        assert!(host.any_op_for("/a/b.txt", kind), "missing {kind}");
    }
}

#[test]
fn rename_is_the_real_atomic_replacement() {
    let host = FakeHost::new();
    host.put_text("/dir/final", "old");
    host.put_text("/dir/.final.tmp-1", "new");
    host.rename(Path::new("/dir/.final.tmp-1"), Path::new("/dir/final"))
        .unwrap();
    assert_eq!(host.file_text("/dir/final").as_deref(), Some("new"));
    assert!(!host.exists("/dir/.final.tmp-1"));
    assert!(host.any_op_for("/dir/.final.tmp-1 -> /dir/final", "rename"));
}

#[test]
fn node_kinds_and_directory_listing_are_real() {
    let host = FakeHost::new();
    host.put_text("/n/file", "12345");
    host.mkdir("/n/dir");
    host.put_link("/n/link", "/n/file");

    let file = host.lstat(Path::new("/n/file")).unwrap();
    assert!(file.is_file() && file.len() == 5);
    assert!(!file.is_dir() && !file.is_symlink());
    let dir = host.lstat(Path::new("/n/dir")).unwrap();
    assert!(dir.is_dir());
    assert!(!dir.is_file() && !dir.is_symlink());
    let link = host.lstat(Path::new("/n/link")).unwrap();
    assert!(link.is_symlink());
    assert!(!link.is_file() && !link.is_dir());
    assert_eq!(link.len(), "/n/file".len() as u64);
    assert_eq!(
        host.lstat(Path::new("/n/missing")).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        host.read_link(Path::new("/n/link")).unwrap(),
        std::path::PathBuf::from("/n/file")
    );

    let mut names: Vec<String> = host
        .read_dir(Path::new("/n"))
        .unwrap()
        .iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["dir", "file", "link"]);
    assert_eq!(
        host.read_dir(Path::new("/n/missing")).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );

    host.remove_file(Path::new("/n/file")).unwrap();
    assert!(!host.exists("/n/file"));
}

// 2. Fault injection ---------------------------------------------------------

#[test]
fn read_write_and_fsync_failures_are_hittable() {
    let host = FakeHost::new();
    host.put_text("/x/file", "bytes");

    host.fail_read("/x/file");
    assert_eq!(
        host.read(Path::new("/x/file")).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    host.clear_failures();
    assert_eq!(host.read(Path::new("/x/file")).unwrap(), b"bytes");

    host.fail_write("/x/file");
    assert_eq!(
        host.create(Path::new("/x/file"), Create::Truncate, 0o600)
            .err()
            .unwrap()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    host.clear_failures();
    assert!(host
        .create(Path::new("/x/file"), Create::Truncate, 0o600)
        .is_ok());

    host.fail_fsync_dir("/x");
    assert!(host.fsync_dir(Path::new("/x")).is_err());
    host.clear_failures();
    assert!(host.fsync_dir(Path::new("/x")).is_ok());
}

#[test]
fn a_planned_second_read_returns_one_changed_byte() {
    let host = FakeHost::new();
    host.put_text("/y/file", "abcdef");
    host.change_on_read("/y/file", 2);
    assert_eq!(host.read(Path::new("/y/file")).unwrap(), b"abcdef");
    let second = host.read(Path::new("/y/file")).unwrap();
    assert_eq!(second.len(), 6);
    assert_eq!(&second[..5], b"abcde");
    assert_ne!(&second[5..], b"f");
    assert_eq!(host.read(Path::new("/y/file")).unwrap(), b"abcdef");
    assert_eq!(host.file_text("/y/file").as_deref(), Some("abcdef"));
}

#[test]
fn rename_failures_cover_exdev_refusal_and_rollback() {
    let host = FakeHost::new();
    host.put_text("/r/one", "a");
    host.put_text("/r/two", "b");

    host.fail_rename_suffix("one");
    assert_eq!(
        host.rename(Path::new("/r/one"), Path::new("/r/moved"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    host.clear_failures();

    host.fail_rename_exdev("two");
    assert_eq!(
        host.rename(Path::new("/r/two"), Path::new("/r/moved"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::CrossesDevices
    );
    host.clear_failures();

    // The first rename to the target passes, the rollback onto the same target is refused.
    host.put_text("/r/.final.tmp-1", "new");
    host.fail_rename_to_suffix_after("final", 1);
    host.rename(Path::new("/r/.final.tmp-1"), Path::new("/r/final"))
        .unwrap();
    assert!(host
        .rename(Path::new("/r/.final.tmp-2"), Path::new("/r/final"))
        .is_err());
    host.clear_failures();
}

// 3. Two mount tables --------------------------------------------------------

#[test]
fn mount_changes_only_the_host_table() {
    let host = FakeHost::new();
    let before = host.container_mount_table();

    let out = host.run(
        &cmd("mount")
            .arg("-t")
            .arg("zfs")
            .arg("-o")
            .arg("zfsutil,rw")
            .arg(DATASET)
            .arg("/run/truenas-nvidia-driver-manager/usr-rw"),
    );
    assert!(out.exit.ok());

    let host_table = host.mount_table().unwrap();
    assert!(host_table
        .iter()
        .any(|m| m.mount_point == "/run/truenas-nvidia-driver-manager/usr-rw"));
    // The host mountinfo sees it, the container's own mountinfo does not.
    let proc1 = String::from_utf8(host.read(Path::new("/proc/1/mountinfo")).unwrap()).unwrap();
    assert!(proc1.contains("/run/truenas-nvidia-driver-manager/usr-rw"));
    let proc_self =
        String::from_utf8(host.read(Path::new("/proc/self/mountinfo")).unwrap()).unwrap();
    assert!(!proc_self.contains("/run/truenas-nvidia-driver-manager/usr-rw"));
    assert!(proc_self.contains("/var/lib/nvm"));
    assert_eq!(host.container_mount_table(), before);

    let _ = host.run(&cmd("umount").arg("/run/truenas-nvidia-driver-manager/usr-rw"));
    assert!(!host
        .mount_table()
        .unwrap()
        .iter()
        .any(|m| m.mount_point == "/run/truenas-nvidia-driver-manager/usr-rw"));
}

// 4. Dataset readonly --------------------------------------------------------

#[test]
fn the_readonly_property_applies_even_when_zfs_set_reports_failure() {
    let host = FakeHost::new();
    let out = host.run(&cmd("zfs").arg("set").arg("readonly=off").arg(DATASET));
    // The overlay makes `zfs set` exit non-zero although it applied.
    assert_ne!(out.exit.code(), Some(0));
    assert_eq!(host.readonly(), "off");
    let get = host.run(
        &cmd("zfs")
            .arg("get")
            .arg("-H")
            .arg("-o")
            .arg("value")
            .arg("readonly")
            .arg(DATASET),
    );
    assert_eq!(get.stdout_text().trim(), "off");
}

// 5. Modules -----------------------------------------------------------------

#[test]
fn modules_follow_references_and_dependencies() {
    let host = FakeHost::new();
    host.load_module("nvidia", 1, &["nvidia_modeset"]);
    host.load_module("nvidia_modeset", 1, &["nvidia"]);
    let text = String::from_utf8(host.read(Path::new("/proc/modules")).unwrap()).unwrap();
    assert!(text.contains("nvidia 1 1 nvidia_modeset Live"), "{text}");

    host.open_device("nvidia");
    let text = String::from_utf8(host.read(Path::new("/proc/modules")).unwrap()).unwrap();
    assert!(text.contains("nvidia 1 2 nvidia_modeset Live"), "{text}");
    assert!(truenas_nvidia_driver_manager::facts::occupancy(
        &truenas_nvidia_driver_manager::facts::parse_proc_modules(&text).unwrap()
    ));

    host.refuse_unload("nvidia");
    let out = host.run(&cmd("rmmod").arg("nvidia"));
    assert!(!out.exit.ok());
    host.unload_module("nvidia");

    assert!(host.run(&cmd("modprobe").arg("nvidia")).exit.ok());
    assert!(host
        .read(Path::new("/proc/modules"))
        .unwrap()
        .windows(6)
        .any(|w| w == b"nvidia"));

    host.set_module_load_fail("nvidia");
    assert!(host.run(&cmd("modprobe").arg("nvidia")).exit.ok());
    host.unload_module("nvidia");
    let out = host.run(&cmd("modprobe").arg("nvidia"));
    assert!(!out.exit.ok());
    assert!(out.stderr_text().contains("No such device"));

    host.fail_reload("nvidia");
    assert!(!host.run(&cmd("modprobe").arg("nvidia")).exit.ok());
}

// 6. Sysext ------------------------------------------------------------------

#[test]
fn the_sysext_model_tracks_since_and_the_missing_link_case() {
    let host = default_host();
    // The default link points at the canonical path; give the target a real image so the refresh
    // has something to merge.
    host.put_text(CANONICAL_PATH_FOR_TESTS, "image");
    host.set_listing(
        CANONICAL_PATH_FOR_TESTS,
        &canonical_listing(KERNEL, "580.178.04"),
    );
    let before = host.sysext_since();
    let status = host.run(&cmd("systemd-sysext").arg("status").arg("--json=short"));
    assert!(status
        .stdout_text()
        .contains(&format!("\"since\":{before}")));
    assert!(host.run(&cmd("systemd-sysext").arg("refresh")).exit.ok());
    assert_eq!(host.sysext_since(), before + 1);
    let version =
        String::from_utf8(host.read(Path::new("/proc/driver/nvidia/version")).unwrap()).unwrap();
    assert!(
        version.contains("580.173.02"),
        "refresh changed a resident module: {version}"
    );
    assert_eq!(host.run(&cmd("nvidia-smi")).exit, Exit::Code(18));

    // A link to a missing target: the refresh returns 0, /usr has no extension and the SINCE does
    // not advance.
    host.put_link("/run/extensions/nvidia.raw", "/images/missing.raw");
    assert!(host.run(&cmd("systemd-sysext").arg("refresh")).exit.ok());
    let status = host.run(&cmd("systemd-sysext").arg("status").arg("--json=short"));
    assert!(status.stdout_text().contains("\"extensions\":\"none\""));
    assert_eq!(host.sysext_since(), before + 1);

    host.put_link("/run/extensions/nvidia.raw", CANONICAL_PATH_FOR_TESTS);
    host.set_refresh_missing(true);
    let status = host.run(&cmd("systemd-sysext").arg("status").arg("--json=short"));
    assert!(status.stdout_text().contains("\"extensions\":\"none\""));
    let before = host.sysext_since();
    assert!(host.run(&cmd("systemd-sysext").arg("refresh")).exit.ok());
    assert_eq!(host.sysext_since(), before);
}

// 7. PCI devices -------------------------------------------------------------

#[test]
fn pci_devices_are_real_files_and_links() {
    let host = FakeHost::new();
    host.add_gpu("0000:01:00.0", 0x1b80, Some("nvidia"));
    assert!(host
        .sandbox_root()
        .join("sys/bus/pci/devices/0000:01:00.0/vendor")
        .exists());
    assert_eq!(
        host.file_text("/sys/bus/pci/devices/0000:01:00.0/vendor")
            .as_deref(),
        Some("0x10de\n")
    );
    assert_eq!(
        host.read_link(Path::new("/sys/bus/pci/devices/0000:01:00.0/driver"))
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy(),
        "nvidia"
    );
    host.set_gpu_driver("0000:01:00.0", Some("vfio-pci"));
    assert_eq!(
        host.read_link(Path::new("/sys/bus/pci/devices/0000:01:00.0/driver"))
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy(),
        "vfio-pci"
    );
    host.set_gpu_driver("0000:01:00.0", None);
    assert_eq!(
        host.read_link(Path::new("/sys/bus/pci/devices/0000:01:00.0/driver"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
}

// 8. Driver readings ---------------------------------------------------------

#[test]
fn driver_readings_follow_the_module_and_the_image() {
    let host = FakeHost::new();
    assert_eq!(
        host.read(Path::new("/proc/driver/nvidia/version"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(
        host.run(&cmd("nvidia-smi").arg("--query-gpu=driver_version"))
            .exit,
        Exit::Code(9)
    );

    host.load_module("nvidia", 0, &[]);
    host.set_driver_version("580.178.04");
    let version =
        String::from_utf8(host.read(Path::new("/proc/driver/nvidia/version")).unwrap()).unwrap();
    assert!(version.contains("580.178.04"), "{version}");
    assert!(version.contains("Open Kernel Module"), "{version}");
    let smi = host.run(&cmd("nvidia-smi").arg("--query-gpu=driver_version"));
    assert_eq!(smi.stdout_text().trim(), "580.178.04");
}

// 9. Commands ----------------------------------------------------------------

#[test]
fn presets_match_by_argv_prefix_and_time_out_at_the_deadline() {
    let host = FakeHost::new();
    host.preset("tool", &["a"], FakeOutput::stdout("first"));
    host.preset("tool", &["a", "b"], FakeOutput::stdout("second"));
    assert_eq!(
        host.run(&cmd("tool").arg("a").arg("b")).stdout_text(),
        "second"
    );
    assert_eq!(host.run(&cmd("tool").arg("a")).stdout_text(), "first");
    assert_eq!(host.run(&cmd("tool").arg("z")).stdout_text(), "");
    assert_eq!(host.count("tool"), 3);
    assert_eq!(host.argv_log()[0], ["tool", "a", "b"]);
    host.assert_red_lines();

    host.preset(
        "slow",
        &[],
        FakeOutput {
            exit: Exit::Code(0),
            stdout: b"late".to_vec(),
            stderr: Vec::new(),
            delay: std::time::Duration::from_secs(60),
        },
    );
    let out = host.run(&Cmd::new(
        At::Host,
        "slow",
        std::time::Duration::from_millis(30),
    ));
    assert_eq!(out.exit, Exit::TimedOut);
    assert!(out.elapsed <= std::time::Duration::from_millis(80));

    // A host command is recorded with the fixed environment.
    let recorded = host.commands();
    assert_eq!(
        recorded[0].env.get("PATH").map(String::as_str),
        Some("/usr/sbin:/usr/bin:/sbin:/bin")
    );
}

// 10. Units and reboot -------------------------------------------------------

#[test]
fn units_disappear_and_run_is_cleared_on_a_reboot() {
    let host = default_host();
    host.set_unit(
        "20260928T101530Z-3f9a0c12",
        "active",
        "running",
        "success",
        0,
    );
    assert!(host
        .run(
            &cmd("systemctl")
                .arg("show")
                .arg("nvm-task-20260928T101530Z-3f9a0c12.service")
                .arg("--property=LoadState,ActiveState,SubState,Result,ExecMainStatus")
        )
        .stdout_text()
        .contains("ActiveState=active"));
    host.put_text("/run/leftover", "x");

    host.reboot_host();
    assert!(host.unit_ids().is_empty());
    assert!(!host.exists("/run/leftover"));
    assert!(host
        .run(
            &cmd("systemctl")
                .arg("show")
                .arg("nvm-task-20260928T101530Z-3f9a0c12.service")
                .arg("--property=LoadState")
        )
        .stdout_text()
        .contains("LoadState=not-found"));
}

// 11. Clock ------------------------------------------------------------------

#[test]
fn the_clock_moves_both_readings() {
    let host = FakeHost::new();
    let wall = host.wall_ms();
    let mono = host.now();
    host.advance(std::time::Duration::from_secs(12 * 3600));
    assert_eq!(host.wall_ms(), wall + 12 * 3600 * 1000);
    assert!(host.now() >= mono + std::time::Duration::from_secs(12 * 3600));
}

// 12. Red line ---------------------------------------------------------------

#[test]
fn the_red_line_catches_the_two_forbidden_commands() {
    let host = FakeHost::new();
    host.preset("docker", &["run"], FakeOutput::ok());
    assert!(host.run(&cmd("docker").arg("run").arg("--rm")).exit.ok());
    host.assert_red_lines();

    let host = FakeHost::new();
    host.preset(
        "systemd-run",
        &[],
        FakeOutput::stdout(&format!("--{}", concat!("col", "lect"))),
    );
    let out = host.run(&cmd("systemd-run").arg(concat!("--", "collect")));
    assert!(out.exit.ok());
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.assert_red_lines()));
    assert!(
        caught.is_err(),
        "the red-line assertion must catch --collect"
    );

    let host = FakeHost::new();
    host.preset("docker", &[], FakeOutput::ok());
    host.run(&cmd("docker").arg("update").arg("nvidia"));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.assert_red_lines()));
    assert!(
        caught.is_err(),
        "the red-line assertion must catch docker.update nvidia"
    );

    let host = FakeHost::new();
    host.forbid_in_argv("hunter12");
    host.preset("tool", &[], FakeOutput::ok());
    host.run(&cmd("tool").arg("hunter12"));
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.assert_red_lines()));
    assert!(
        caught.is_err(),
        "the red-line assertion must catch a registered secret"
    );

    let caught = std::panic::catch_unwind(|| {
        let host = FakeHost::new();
        host.preset("systemd-run", &[], FakeOutput::ok());
        host.run(&cmd("systemd-run").arg(concat!("--", "collect")));
        drop(host);
    });
    assert!(
        caught.is_err(),
        "a FakeHost was dropped without asserting the red lines"
    );
}

// 13. dpkg-deb passthrough ---------------------------------------------------

fn dpkg_deb_command() -> std::process::Command {
    // FakeHost's real dpkg-deb passthrough uses the host-command environment. Probe and build with
    // that same PATH, regardless of the test runner's PATH.
    let mut command = std::process::Command::new("dpkg-deb");
    command.env_clear();
    command.env("LC_ALL", "C");
    command.env("PATH", HOST_PATH);
    command
}

#[test]
fn dpkg_deb_passthrough_runs_the_real_program_when_it_exists() {
    let available = dpkg_deb_command().arg("--version").output().is_ok();
    if !available {
        // No dpkg-deb on the host-command PATH: the real-program path is exercised in the CI image.
        // The preset path is asserted in upload.rs.
        eprintln!("dpkg-deb not on host-command PATH: passthrough stays UNVERIFIED here");
        return;
    }
    let dir = std::env::temp_dir().join(format!("nvm-dpkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let pkg = dir.join("pkg");
    std::fs::create_dir_all(pkg.join("DEBIAN")).unwrap();
    // The CI runner can use umask 000; dpkg-deb rejects a 0777 control dir.
    std::fs::set_permissions(pkg.join("DEBIAN"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let control = pkg.join("DEBIAN/control");
    std::fs::write(
        &control,
        "Package: libnvidia-container1\nVersion: 1.20.1-1\nArchitecture: amd64\nMaintainer: t <t@example.com>\nDescription: t\n",
    )
    .unwrap();
    std::fs::set_permissions(&control, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::write(pkg.join("payload"), b"x").unwrap();
    let deb = dir.join("libnvidia-container1_1.20.1-1_amd64.deb");
    let built = dpkg_deb_command()
        .arg("--build")
        .arg(&pkg)
        .arg(&deb)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );

    let host = FakeHost::new();
    host.put_file(deb.to_str().unwrap(), std::fs::read(&deb).unwrap());
    host.set_dpkg_passthrough(true);
    let out = host.run(
        &cmd("dpkg-deb")
            .arg("--field")
            .arg(deb.as_os_str())
            .arg("Package")
            .arg("Version")
            .arg("Architecture"),
    );
    assert!(out.exit.ok(), "{}", out.stderr_text());
    let text = out.stdout_text();
    assert!(text.contains("Package: libnvidia-container1"), "{text}");
    assert!(text.contains("Version: 1.20.1-1"), "{text}");
    assert!(text.contains("Architecture: amd64"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

// 14. Command timeout through a blocking reader ------------------------------

#[test]
fn a_child_holding_the_pipe_does_not_extend_a_deadline_in_the_fake() {
    // The fake host has no real child processes; a preset past the deadline reports TimedOut (see
    // the command test above). The real process-group behaviour is asserted in host.rs against the
    // real `Host`.
    let host = FakeHost::new();
    host.preset(
        "sleepy",
        &[],
        FakeOutput {
            exit: Exit::Code(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
            delay: std::time::Duration::from_millis(200),
        },
    );
    let out = host.run(&Cmd::new(
        At::Host,
        "sleepy",
        std::time::Duration::from_millis(20),
    ));
    assert_eq!(out.exit, Exit::TimedOut);
}

// 15. read_changes also apply to open --------------------------------------

#[test]
fn a_changed_read_applies_to_the_stream_interface_too() {
    let host = FakeHost::new();
    host.put_text("/z/file", "abc");
    host.change_on_read("/z/file", 1);
    let mut reader = host.open(Path::new("/z/file")).unwrap();
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).unwrap();
    assert_eq!(buf, b"abb");
}

#[test]
fn an_absolute_symlink_reads_the_virtual_mapped_target() {
    let host = FakeHost::new();
    // A real file outside the sandbox carrying different bytes: the sandbox copy is the only thing
    // a virtual read may see.
    let outside = std::env::temp_dir().join(format!(
        "nvm-controlled-target-{}",
        truenas_nvidia_driver_manager::store::random_hex(8)
    ));
    std::fs::write(&outside, b"real host bytes outside the sandbox").unwrap();
    host.put_text(outside.to_str().unwrap(), "virtual mapped target");
    host.put_link("/virtual-link", outside.to_str().unwrap());
    let read = host.read(Path::new("/virtual-link")).unwrap();
    let _ = std::fs::remove_file(&outside);
    assert_eq!(
        read, b"virtual mapped target",
        "a read escaped the virtual mapping"
    );
}

#[test]
fn the_web_process_self_mounts_is_the_container_table() {
    let host = FakeHost::new();
    host.add_mount("pool/usr", "/usr", "zfs", "ro");
    host.add_container_mount("overlay", "/", "overlay", "rw");
    let web = String::from_utf8(host.read(Path::new("/proc/self/mounts")).unwrap()).unwrap();
    let host_table = String::from_utf8(host.read(Path::new("/proc/1/mountinfo")).unwrap()).unwrap();
    assert!(web.lines().any(|l| l.starts_with("overlay / overlay")));
    assert!(!web.lines().any(|l| l.starts_with("pool/usr /usr zfs")));
    assert!(host_table.contains("/usr"));

    // Inside a runner scope the same path is the host table; the guard restores the web view
    // afterwards.
    {
        let _scope = host.enter_runner_scope();
        let runner = String::from_utf8(host.read(Path::new("/proc/self/mounts")).unwrap()).unwrap();
        assert!(runner.lines().any(|l| l.starts_with("pool/usr /usr zfs")));
        assert!(!runner.lines().any(|l| l.starts_with("overlay / overlay")));
    }
    let after = String::from_utf8(host.read(Path::new("/proc/self/mounts")).unwrap()).unwrap();
    assert!(after.lines().any(|l| l.starts_with("overlay / overlay")));
}

#[test]
fn rmmod_refuses_a_holder_or_an_open_device() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &["nvidia_uvm"]);
    host.load_module("nvidia_uvm", 0, &[]);
    let out = host.run(&cmd("rmmod").arg("nvidia"));
    assert!(!out.exit.ok(), "the model removed a module with a holder");
    assert!(out.stderr_text().contains("in use"), "{out:?}");

    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.open_device("nvidia");
    let out = host.run(&cmd("rmmod").arg("nvidia"));
    assert!(!out.exit.ok(), "the model ignored an open-device reference");

    host.unload_module("nvidia");
    host.load_module("nvidia", 0, &[]);
    assert!(host.run(&cmd("rmmod").arg("nvidia")).exit.ok());
    assert!(!host
        .read(Path::new("/proc/modules"))
        .unwrap()
        .windows(6)
        .any(|w| w == b"nvidia"));
}

#[test]
fn refresh_merges_the_link_target_image() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.set_driver_version("580.173.02");
    let target = "/images/new.raw";
    host.put_text(target, "new image");
    host.set_listing(target, &canonical_listing(KERNEL, "580.178.04"));
    host.put_link("/run/extensions/nvidia.raw", target);
    let before = host.sysext_since();
    assert!(host.run(&cmd("systemd-sysext").arg("refresh")).exit.ok());
    assert_ne!(host.sysext_since(), before);
    assert!(host.run(&cmd("modprobe").arg("nvidia")).exit.ok());
    let resident =
        String::from_utf8(host.read(Path::new("/proc/driver/nvidia/version")).unwrap()).unwrap();
    assert!(resident.contains("580.173.02"));
    assert_eq!(host.run(&cmd("nvidia-smi")).exit, Exit::Code(18));

    assert!(host.run(&cmd("rmmod").arg("nvidia")).exit.ok());
    assert!(host.read(Path::new("/proc/driver/nvidia/version")).is_err());
    assert!(host.run(&cmd("systemd-sysext").arg("refresh")).exit.ok());
    assert!(host.run(&cmd("modprobe").arg("nvidia")).exit.ok());
    let version =
        String::from_utf8(host.read(Path::new("/proc/driver/nvidia/version")).unwrap()).unwrap();
    assert!(version.contains("580.178.04"), "{version}");
}

#[test]
fn a_reboot_reloads_the_modules_from_the_canonical_image() {
    let host = FakeHost::new();
    host.load_module("nvidia", 0, &[]);
    host.set_driver_version("580.173.02");
    host.put_text(CANONICAL_PATH_FOR_TESTS, "canonical image");
    host.set_listing(
        CANONICAL_PATH_FOR_TESTS,
        &canonical_listing(KERNEL, "580.178.04"),
    );
    host.put_text("/run/probe", "gone after boot");
    host.reboot_host();
    assert!(!host.exists("/run/probe"));
    assert!(host
        .read(Path::new("/proc/modules"))
        .unwrap()
        .windows(6)
        .any(|w| w == b"nvidia"));
    let version =
        String::from_utf8(host.read(Path::new("/proc/driver/nvidia/version")).unwrap()).unwrap();
    assert!(version.contains("580.178.04"), "{version}");
}

fn run_args(
    host: &FakeHost,
    program: &'static str,
    args: &[&str],
) -> truenas_nvidia_driver_manager::host::Output {
    let mut cmd = Cmd::new(At::Host, program, std::time::Duration::from_secs(5));
    cmd.args = args.iter().map(std::ffi::OsString::from).collect();
    host.run(&cmd)
}

#[test]
fn two_handles_contend_for_the_same_modelled_lock() {
    let host = FakeHost::new();
    let container = "/var/lib/nvm/manager/control/drain.lock";
    let host_view = format!("{HOST_DATA_DIR}/manager/control/drain.lock");
    let proc_view = "/proc/1/root/var/lib/nvm/manager/control/drain.lock";

    let mut first = host.open_lock(Path::new(container), 0o600).unwrap();
    assert!(first.try_acquire().unwrap());
    assert!(host.lock_held(container));
    assert!(host.lock_held(&host_view));
    assert!(host.lock_held(proc_view));

    // A second handle of the SAME process still contends, and a repeated acquire of the holder is a
    // harmless success.
    let mut second = host.open_lock(Path::new(proc_view), 0o600).unwrap();
    assert!(!second.try_acquire().unwrap());
    assert!(first.try_acquire().unwrap());

    first.release().unwrap();
    assert!(!host.lock_held(container));
    assert!(second.try_acquire().unwrap());
    // Releasing twice is fine, and `Drop` releases what is still held.
    second.release().unwrap();
    second.release().unwrap();
    assert!(!host.lock_held(container));
}

#[test]
fn the_lock_identity_comes_from_the_sandbox_node() {
    let host = FakeHost::new();
    let lock = host.open_lock(Path::new("/run/x.lock"), 0o600).unwrap();
    assert_ne!(
        lock.identity().unwrap(),
        truenas_nvidia_driver_manager::host::LockId { dev: 0, ino: 0 }
    );
    assert!(host.exists("/run/x.lock"));
}

#[test]
fn the_injected_lock_modes_and_failures_are_honoured() {
    let host = FakeHost::new();
    host.set_lock_mode(0o666);
    let error = host
        .open_lock(Path::new("/run/wide.lock"), 0o600)
        .err()
        .expect("a wide lock mode must be refused");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(host.exists("/run/wide.lock"));
    assert!(!host.lock_held("/run/wide.lock"));

    host.set_lock_mode(0o600);
    let mut lock = host.open_lock(Path::new("/run/wide.lock"), 0o600).unwrap();
    host.set_lock_acquire_fails(true);
    assert!(lock.try_acquire().is_err());
    host.set_lock_acquire_fails(false);
    assert!(lock.try_acquire().unwrap());
    host.set_lock_open_fails(true);
    assert!(host.open_lock(Path::new("/run/y.lock"), 0o600).is_err());
}

#[test]
fn docker_ps_answers_only_the_modelled_forms() {
    let host = FakeHost::new();
    host.add_container("nvm-build-one", false);
    host.add_container("nvm-build-two", true);
    let id = "a".repeat(64);
    host.set_docker_ps(&[id.as_str()]);

    let ids = run_args(&host, "docker", &["ps", "-q", "--no-trunc"]);
    assert!(ids.exit.ok());
    assert_eq!(ids.stdout_text(), id);

    for args in [
        vec!["ps", "-a", "--no-trunc", "--format", "{{.Names}}"],
        vec!["ps", "-a", "--format", "\"{{.Names}}\""],
        vec!["ps", "-a", "--format={{.Names}}"],
    ] {
        let names = run_args(&host, "docker", &args);
        assert!(names.exit.ok(), "{args:?}: {}", names.stderr_text());
        assert_eq!(names.stdout_text(), "nvm-build-one\nnvm-build-two\n");
    }

    // A shape the model does not answer goes red instead of guessing.
    for args in [
        vec!["ps", "-a", "--format", "{{.ID}}"],
        vec!["ps", "-a"],
        vec!["ps", "--format", "{{.Names}}"],
        vec!["bogus"],
    ] {
        let out = run_args(&host, "docker", &args);
        assert!(!out.exit.ok(), "{args:?} must not succeed");
        assert!(!out.stderr_text().is_empty(), "{args:?} needs a reason");
    }
}

#[test]
fn docker_rm_can_noop_or_fail_without_removing() {
    let host = FakeHost::new();
    host.add_container("nvm-build-x", false);
    host.set_docker_rm_noop(true);
    let noop = run_args(&host, "docker", &["rm", "-f", "nvm-build-x"]);
    assert!(noop.exit.ok());
    assert!(host.has_build_container("nvm-build-x"));

    host.set_docker_rm_noop(false);
    host.set_docker_rm_fails(true);
    let failed = run_args(&host, "docker", &["rm", "-f", "nvm-build-x"]);
    assert!(!failed.exit.ok());
    assert!(host.has_build_container("nvm-build-x"));

    host.set_docker_rm_fails(false);
    assert!(run_args(&host, "docker", &["rm", "-f", "nvm-build-x"])
        .exit
        .ok());
    assert!(!host.has_build_container("nvm-build-x"));
}

#[test]
fn systemctl_list_jobs_and_unmodelled_subcommands() {
    let host = FakeHost::new();
    let job = "123 nvm-task-20260101T000000Z-aaaaaaaa.service start running";
    host.set_jobs(&[job]);
    let out = run_args(&host, "systemctl", &["list-jobs"]);
    assert!(out.exit.ok());
    assert_eq!(out.stdout_text(), format!("{job}\n"));

    let empty = FakeHost::new();
    let out = run_args(&empty, "systemctl", &["list-jobs"]);
    assert!(out.exit.ok());
    assert_eq!(out.stdout_text(), "");

    for args in [vec!["frobnicate"], vec!["start", "nvm-task-x.service"]] {
        let out = run_args(&host, "systemctl", &args);
        assert!(!out.exit.ok(), "{args:?} must not succeed");
        assert!(!out.stderr_text().is_empty(), "{args:?} needs a reason");
    }
    assert!(run_args(&host, "systemctl", &["reset-failed"]).exit.ok());
}

#[test]
fn docker_ps_all_partial_writes_a_cut_off_name() {
    let host = FakeHost::new();
    host.add_container("nvm-build-partial", false);
    host.set_docker_ps_all_fails(true);
    host.set_docker_ps_all_partial(true);
    let out = run_args(&host, "docker", &["ps", "-a", "--format", "{{.Names}}"]);
    assert!(!out.exit.ok());
    assert_eq!(out.stdout_text(), "nvm-build-pa\n");
    host.set_docker_ps_all_times_out(true);
    let timed_out = run_args(&host, "docker", &["ps", "-a", "--format", "{{.Names}}"]);
    assert_eq!(timed_out.exit, Exit::TimedOut);
}

#[test]
fn the_admission_control_lock_works_on_the_fake() {
    let host = default_host();
    let mut lock = truenas_nvidia_driver_manager::admission::control_lock(
        &host,
        std::time::Duration::from_secs(1),
    )
    .expect("the fake host must be able to take the admission control lock");
    assert!(host.lock_held("/run/nvm-admission/control.lock"));
    let blocked =
        truenas_nvidia_driver_manager::admission::control_lock(&host, std::time::Duration::ZERO);
    assert!(blocked.is_err(), "a second control lock must not be given");
    lock.release().unwrap();
    assert!(!host.lock_held("/run/nvm-admission/control.lock"));
}

#[test]
fn a_reboot_clears_the_containers_jobs_and_locks() {
    let host = FakeHost::new();
    host.add_container("nvm-build-x", true);
    host.set_jobs(&["123 nvm-task-x.service start running"]);
    let mut lock = host
        .open_lock(Path::new("/run/reboot.lock"), 0o600)
        .unwrap();
    assert!(lock.try_acquire().unwrap());

    host.reboot_host();
    assert!(host.container_names().is_empty());
    assert!(!host.lock_held("/run/reboot.lock"));
    let jobs = run_args(&host, "systemctl", &["list-jobs"]);
    assert!(jobs.exit.ok());
    assert_eq!(jobs.stdout_text(), "");
}

/// Follow redirects with both protocol restrictions and the final URL last.
fn curl_follow(start: &str, redir: &str) -> Cmd {
    Cmd::new(At::Host, "curl", std::time::Duration::from_secs(10))
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            redir,
            "--max-redirs",
            "5",
            "--max-time",
            "10",
            "--write-out",
            "%{http_code}",
            "--output",
            "/tmp/result",
        ])
        .arg(start)
}

#[test]
fn redirect_protocols_cannot_override_transfer_protocols() {
    // `--proto =https` already forbids http. `--proto-redir =http` must not turn it back on. The
    // http target holds a full body, so following it would look like success.
    let host = FakeHost::new();
    let start = "https://example.test/start";
    let end = "http://example.test/end";
    host.set_curl_redirect(start, 302, end, b"redirect");
    host.set_curl(end, 200, b"final bytes");
    let out = host.run(&curl_follow(start, "=http"));
    assert_eq!(out.exit, Exit::Code(1));
    assert_eq!(out.stdout_text(), "302");
    assert_eq!(
        out.stderr_text(),
        "curl: (1) Protocol \"http\" disabled (in redirect)\n"
    );
    assert!(!host.exists("/tmp/result"));
}

#[test]
fn a_gate_wait_observes_the_barrier_or_a_finished_runner() {
    let gate = std::sync::Arc::new(common::fake_host::Gate::default());
    let finished = std::thread::spawn(|| ());
    assert!(!gate.wait_blocked(&finished));
    finished.join().unwrap();

    let worker_gate = gate.clone();
    let runner = std::thread::spawn(move || worker_gate.wait_observed());
    let blocked = gate.wait_blocked(&runner);
    gate.release();
    runner.join().unwrap();
    assert!(blocked);
}
