//! String keys and constants shared by the web process and the runner.
//!
//! Interfaces send stable reason keys and parameters for the frontend to render.

use std::time::Duration;

pub const SESSION_KEY_BYTES: usize = 32;
pub const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
pub const SESSION_MAX_AGE: u64 = 12 * 60 * 60;
pub const LOGIN_QUOTA: u32 = 5;
pub const LOGIN_REFILL: Duration = Duration::from_secs(30);
pub const LOGIN_RESET_HELP_AFTER: u32 = 3;

pub const SSH_NAMESPACE: &str = "nvm-login";
pub const SSH_CHALLENGE_BYTES: usize = 32;
pub const SSH_CHALLENGE_TTL: Duration = Duration::from_secs(120);
pub const SSH_CHALLENGE_MAX_OUTSTANDING: usize = 256;
pub const SSH_CHALLENGE_ISSUE_QUOTA: u32 = 10;
pub const SSH_CHALLENGE_ISSUE_REFILL: Duration = Duration::from_secs(10);
pub const SSH_MAX_KEYS: usize = 5;
pub const SSH_KEY_LINE_MAX: usize = 8 * 1024;
pub const SSH_COMMENT_MAX: usize = 1024;
pub const SSH_SIGNATURE_MAX: usize = 16 * 1024;
pub const SSH_CHALLENGE_ID_MAX: usize = 64;

pub const REPLACE_DEADLINE: Duration = Duration::from_secs(600);
pub const REPLACE_DEADLINE_MINUTES: u64 = 10;

pub const READING_TIMEOUT: Duration = Duration::from_secs(15);
pub const HOST_QUERY_TIMEOUT: Duration = Duration::from_secs(30);
pub const HOST_CMD_TIMEOUT: Duration = Duration::from_secs(180);
pub const CATALOG_CONNECT: Duration = Duration::from_secs(5);
pub const CATALOG_TOTAL: Duration = Duration::from_secs(15);
pub const CATALOG_CMD_TIMEOUT: Duration = Duration::from_secs(20);

pub const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const RUN_HEADER_WINDOW: usize = 64 * 1024;
pub const UPLOAD_FILE_LIMIT: u64 = 1024 * 1024 * 1024;

pub const DOWNLOAD_MIN_REMAINING: Duration = Duration::from_secs(1);
pub const DOWNLOAD_PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
pub const ERROR_RAW_LINES: usize = 100;
pub const LOG_TAIL_LINES: usize = 200;
pub const FINGERPRINT_HEX: usize = 12;
pub const VERSION_SEGMENT_DIGITS: usize = 9;
pub const COPY_CHUNK_BYTES: usize = 1024 * 1024;
/// Anonymous JSON body limit. A larger body is refused before it is parsed.
/// SSH signatures (16 KiB) and public-key lines (8 KiB) fit under it;
/// file upload routes use their own streaming limits.
pub const JSON_BODY_LIMIT: usize = 64 * 1024;
/// How long a JSON body may take to arrive before the read is refused.
pub const JSON_BODY_DEADLINE: Duration = Duration::from_secs(10);

const _: () = {
    assert!(SSH_SIGNATURE_MAX < JSON_BODY_LIMIT);
    assert!(SSH_KEY_LINE_MAX < JSON_BODY_LIMIT);
    assert!(
        SSH_SIGNATURE_MAX + SSH_KEY_LINE_MAX + SSH_COMMENT_MAX + SSH_CHALLENGE_ID_MAX + 512
            < JSON_BODY_LIMIT
    );
};
pub const CLIENT_NAME_MAX: usize = 255;
pub const TEMP_SUFFIX_HEX: usize = 16;
pub const STARTUP_CHECK_HEX: usize = 32;
pub const TASK_ID_SUFFIX_HEX: usize = 8;
pub const GROUP_ID_HEX: usize = 32;

pub const ARGON2_M_COST: u32 = 19456;
pub const ARGON2_T_COST: u32 = 2;
pub const ARGON2_P_COST: u32 = 1;
pub const ARGON2_SALT_BYTES: usize = 16;
pub const ARGON2_OUTPUT_BYTES: usize = 32;

pub const CANONICAL_PATH: &str = "/usr/share/truenas/sysext-extensions/nvidia.raw";
/// Extension link checked by the middleware.
pub const LINK_PATH: &str = "/run/extensions/nvidia.raw";
pub const CANONICAL_DIR: &str = "/usr/share/truenas/sysext-extensions";
pub const RW_MOUNT: &str = "/run/truenas-nvidia-driver-manager/usr-rw";
pub const CANONICAL_TMP_NAME: &str = ".nvidia.raw.nvm-tmp";
pub const RW_TMP_PATH: &str =
    "/run/truenas-nvidia-driver-manager/usr-rw/share/truenas/sysext-extensions/.nvidia.raw.nvm-tmp";
pub const RW_CANONICAL_PATH: &str =
    "/run/truenas-nvidia-driver-manager/usr-rw/share/truenas/sysext-extensions/nvidia.raw";

// ---- Host-global locks ------------------------------------------------------

/// Host-global directory of the two locks. It is its own directory on purpose:
/// another code path removes the writable-mount parent when it happens to be
/// empty, and a lock must not live under a directory that can be deleted before
/// the lock file is created.
pub const LOCK_DIR: &str = "/run/nvm-admission";
/// Short-transaction lock serializing submission and the disk-changing part of
/// reconciliation.
pub const CONTROL_LOCK_NAME: &str = "control.lock";
pub const EXECUTION_LOCK_NAME: &str = "execution.lock";
pub const LOCK_DIR_MODE: u32 = 0o700;
pub const PRIVATE_FILE_MODE: u32 = 0o600;

// Lock reasons. The `admission.` prefix is kept: it is the stable spelling of
// the lock protocol, not a deployment-admission state.
pub const ADMISSION_LOCK_UNSUPPORTED: &str = "admission.lock_unsupported";
pub const ADMISSION_LOCK_DIR_INVALID: &str = "admission.lock_dir_invalid";
pub const ADMISSION_LOCK_BUSY: &str = "admission.lock_busy";
pub const ADMISSION_LOCK_TIMEOUT: &str = "admission.lock_timeout";

// Inventory reasons of the shared pre-inventory.
pub const DRAIN_PENDING_JOB: &str = "drain.pending_job";
pub const DRAIN_STATE_SCAN_FAILED: &str = "drain.state_scan_failed";
pub const DRAIN_UNEXPLAINED_UNIT_ROW: &str = "drain.unexplained_unit_row";

// Runner refusals (machine-readable reasons never enter the task state).
pub const RUNNER_DUPLICATE_EXECUTION: &str = "runner.duplicate_execution";
pub const RUNNER_STATE_NOT_RUNNING: &str = "runner.state_not_running";
pub const RUNNER_ALREADY_STARTED: &str = "runner.already_started";
pub const CONTAINER_DATA_DIR: &str = "/var/lib/nvm";
pub const KERNEL_HEADERS_PATH: &str = "/usr/src/linux-headers-truenas-production-amd64";
pub const WEB_ROOT: &str = "/usr/local/share/truenas-nvidia-driver-manager/web";
pub const MANIFEST_PATH: &str = "/data/manifest.json";
pub const NVRM_VERSION_PATH: &str = "/proc/driver/nvidia/version";
pub const PROC_MODULES_PATH: &str = "/proc/modules";
pub const PCI_DEVICES_PATH: &str = "/sys/bus/pci/devices";
pub const TOOLKIT_INDEX_URL: &str =
    "https://nvidia.github.io/libnvidia-container/stable/deb/amd64/Packages";
pub const CATALOG_URL: &str = "https://download.nvidia.com/XFree86/Linux-x86_64/";
/// The sole official 470 installer qualified for the fixed Debian patch input.
pub const COMMUNITY_PATCH_VERSION: &str = "470.256.02";
pub const COMMUNITY_PATCH_RUN_BYTES: u64 = 223_168_377;
pub const COMMUNITY_PATCH_RUN_SHA256: &str =
    "fe8f58732055dacc4af0c4bb2371022d6e116e9f9594d7d3bea71f5a8a29e2b1";
pub const COMMUNITY_PATCH_ARCHIVE_NAME: &str =
    "nvidia-graphics-drivers-tesla-470_470.256.02-5~bpo12+1.debian.tar.xz";
pub const COMMUNITY_PATCH_INPUT_NAME: &str = "community-patches.debian.tar.xz";
pub const COMMUNITY_PATCH_ARCHIVE_URL: &str = "https://deb.debian.org/debian/pool/non-free/n/nvidia-graphics-drivers-tesla-470/nvidia-graphics-drivers-tesla-470_470.256.02-5~bpo12+1.debian.tar.xz";
pub const COMMUNITY_PATCH_ARCHIVE_BYTES: u64 = 225_888;
pub const COMMUNITY_PATCH_ARCHIVE_SHA256: &str =
    "3aeddf4e778ef10535de28dbfa613b24bcf334767c0ac6473193b840ec48d973";
pub const TOOLKIT_PACKAGES: [&str; 4] = [
    "libnvidia-container1",
    "libnvidia-container-tools",
    "nvidia-container-toolkit-base",
    "nvidia-container-toolkit",
];
/// Lowest toolkit upstream version shown as below the security floor.
pub const TOOLKIT_SECURITY_FLOOR: [u32; 3] = [1, 19, 1];
pub const TOOLKIT_DRIVER_615_FLOOR: [u32; 3] = [1, 20, 1];
/// First driver branch requiring the higher toolkit floor.
pub const TOOLKIT_DRIVER_FLOOR_BRANCH: u32 = 615;
pub const MIN_MAJOR_BRANCH: u32 = 470;
/// Unsupported optional installer flags are logged as `NVM:INFO:flag_unsupported=<flag>`.
/// The module-type flag has its own branch gate and is not probed here.
pub const PROBE_FLAGS: [&str; 8] = [
    "--no-x-check",
    "--no-nouveau-check",
    "--no-backup",
    "--no-systemd",
    "--install-libglvnd",
    "--skip-module-load",
    "--no-rebuild-initramfs",
    "--allow-installation-with-running-driver",
];
pub const MODULE_TYPE_MIN_BRANCH: u32 = 555;

/// The twelve error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    SaveOriginalFailed,
    DownloadFailed,
    BuildFailed,
    BuildTimeout,
    GpuInUse,
    ModuleUnloadRefused,
    WriteFailed,
    ReplaceDeadlineExceeded,
    RestoreAborted,
    RestoreNotWritten,
    RestoreCheckFailed,
    NoUsableOriginal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::SaveOriginalFailed => "SAVE_ORIGINAL_FAILED",
            ErrorCode::DownloadFailed => "DOWNLOAD_FAILED",
            ErrorCode::BuildFailed => "BUILD_FAILED",
            ErrorCode::BuildTimeout => "BUILD_TIMEOUT",
            ErrorCode::GpuInUse => "GPU_IN_USE",
            ErrorCode::ModuleUnloadRefused => "MODULE_UNLOAD_REFUSED",
            ErrorCode::WriteFailed => "WRITE_FAILED",
            ErrorCode::ReplaceDeadlineExceeded => "REPLACE_DEADLINE_EXCEEDED",
            ErrorCode::RestoreAborted => "RESTORE_ABORTED",
            ErrorCode::RestoreNotWritten => "RESTORE_NOT_WRITTEN",
            ErrorCode::RestoreCheckFailed => "RESTORE_CHECK_FAILED",
            ErrorCode::NoUsableOriginal => "NO_USABLE_ORIGINAL",
        }
    }

    pub fn parse(s: &str) -> Option<ErrorCode> {
        Some(match s {
            "SAVE_ORIGINAL_FAILED" => ErrorCode::SaveOriginalFailed,
            "DOWNLOAD_FAILED" => ErrorCode::DownloadFailed,
            "BUILD_FAILED" => ErrorCode::BuildFailed,
            "BUILD_TIMEOUT" => ErrorCode::BuildTimeout,
            "GPU_IN_USE" => ErrorCode::GpuInUse,
            "MODULE_UNLOAD_REFUSED" => ErrorCode::ModuleUnloadRefused,
            "WRITE_FAILED" => ErrorCode::WriteFailed,
            "REPLACE_DEADLINE_EXCEEDED" => ErrorCode::ReplaceDeadlineExceeded,
            "RESTORE_ABORTED" => ErrorCode::RestoreAborted,
            "RESTORE_NOT_WRITTEN" => ErrorCode::RestoreNotWritten,
            "RESTORE_CHECK_FAILED" => ErrorCode::RestoreCheckFailed,
            "NO_USABLE_ORIGINAL" => ErrorCode::NoUsableOriginal,
            _ => return None,
        })
    }
}

// ---- Reason keys -----------------------------------------

pub const REASON_FILE_MISSING: &str = "reason.file_missing";
pub const REASON_PERMISSION_DENIED: &str = "reason.permission_denied";
pub const REASON_IO_ERROR: &str = "reason.io_error";
pub const REASON_FORMAT_UNRECOGNIZED: &str = "reason.format_unrecognized";
pub const REASON_RECORD_UNREADABLE: &str = "reason.record_unreadable";
pub const REASON_TIMED_OUT: &str = "reason.timed_out";
pub const REASON_EXIT_STATUS: &str = "reason.exit_status";
pub const REASON_SIGNAL: &str = "reason.signal";
pub const REASON_SPAWN_FAILED: &str = "reason.spawn_failed";
pub const REASON_CONNECT_FAILED: &str = "reason.connect_failed";
pub const REASON_HTTP_STATUS: &str = "reason.http_status";
pub const REASON_INDEX_UNRECOGNIZED: &str = "reason.index_unrecognized";
pub const REASON_STARTUP_CHECK_FAILED: &str = "reason.startup_check_failed";

pub const SAVE_CANONICAL_UNREADABLE: &str = "save.canonical_unreadable";
pub const SAVE_WRITE_FAILED: &str = "save.write_failed";
pub const SAVE_READBACK_MISMATCH: &str = "save.readback_mismatch";
pub const SAVE_FACTS_UNREADABLE: &str = "save.facts_unreadable";

pub const WRITE_DATASET_UNKNOWN: &str = "write.dataset_unknown";
pub const WRITE_READONLY_UNREADABLE: &str = "write.readonly_unreadable";
pub const WRITE_RW_MOUNT_NOT_CONFIRMED: &str = "write.rw_mount_not_confirmed";
pub const WRITE_SOURCE_UNREADABLE: &str = "write.source_unreadable";
pub const WRITE_TEMP_FAILED: &str = "write.temp_failed";
pub const WRITE_TEMP_MISMATCH: &str = "write.temp_mismatch";
pub const WRITE_RENAME_FAILED: &str = "write.rename_failed";

pub const DOWNLOAD_REQUEST_FAILED: &str = "download.request_failed";
pub const DOWNLOAD_HTTP_STATUS: &str = "download.http_status";
pub const DOWNLOAD_TOOLKIT_INDEX_UNREADABLE: &str = "download.toolkit_index_unreadable";
pub const DOWNLOAD_TOOLKIT_NO_COMMON_VERSION: &str = "download.toolkit_no_common_version";
pub const DOWNLOAD_TOOLKIT_TOO_OLD: &str = "download.toolkit_too_old";
pub const DOWNLOAD_TOOLKIT_ENTRY_UNEXPECTED: &str = "download.toolkit_entry_unexpected";
pub const DOWNLOAD_CHECKSUM_MISMATCH: &str = "download.checksum_mismatch";

pub const BRANCH_FOLLOW_PFN_REMOVED: &str = "branch.follow_pfn_removed";
pub const BRANCH_OUTPUT_POLL_CHANGED_REMOVED: &str = "branch.output_poll_changed_removed";
pub const BRANCH_PCI_RESIZE_RESOURCE_ARGS: &str = "branch.pci_resize_resource_args";
pub const BRANCH_INSTALLER_LOADS_MODULE: &str = "branch.installer_loads_module";
pub const BRANCH_NVIDIA_NOT_LOADED: &str = "branch.nvidia_not_loaded";
pub const BRANCH_NVIDIA_DRM_NOT_LOADED: &str = "branch.nvidia_drm_not_loaded";

pub const RESTORE_NOT_SAVED: &str = "restore.no_original.not_saved";
pub const RESTORE_CHECKSUM_MISMATCH: &str = "restore.no_original.checksum_mismatch";
pub const RESTORE_UNREADABLE: &str = "restore.no_original.unreadable";

/// The leftovers a task can leave on the host after its own cleanup could not
/// put everything back. A new task is refused while one of them is read back.
pub const HOST_RW_MOUNT_LEFTOVER: &str = "host.rw_mount_leftover";
pub const HOST_READONLY_LEFTOVER: &str = "host.readonly_leftover";

pub const BUILD_RUN_CHECK_FAILED: &str = "run_check_failed";
pub const BUILD_MODULES_COPY_FAILED: &str = "modules_copy_failed";
pub const BUILD_KERNEL_GCC_UNKNOWN: &str = "kernel_gcc_unknown";
pub const BUILD_COMPILER_MISSING: &str = "compiler_missing";
pub const BUILD_EXTRACT_FAILED: &str = "extract_failed";
pub const BUILD_OPTIONS_UNREADABLE: &str = "options_unreadable";
pub const BUILD_KERNEL_MODULE_TYPE_UNSUPPORTED: &str = "kernel_module_type_unsupported";
pub const BUILD_INSTALLER_FAILED: &str = "installer_failed";
pub const BUILD_INSTALLER_USERSPACE_FAILED: &str = "installer_userspace_failed";
pub const BUILD_INSTALLER_KERNEL_MODULE_FAILED: &str = "installer_kernel_module_failed";
pub const BUILD_DEPMOD_FAILED: &str = "depmod_failed";
pub const BUILD_CAPTURE_EMPTY: &str = "capture_empty";
pub const BUILD_STAGE_COPY_FAILED: &str = "stage_copy_failed";
pub const BUILD_DEB_EXTRACT_FAILED: &str = "deb_extract_failed";
pub const BUILD_RELEASE_WRITE_FAILED: &str = "release_write_failed";
pub const BUILD_MKSQUASHFS_FAILED: &str = "mksquashfs_failed";
pub const BUILD_RESULT_WRITE_FAILED: &str = "result_write_failed";
pub const BUILD_UNEXPECTED: &str = "unexpected";
pub const BUILD_COMMUNITY_PATCH_FAILED: &str = "community_patch_failed";
pub const BUILD_KERNEL_MODULE_BUILD_FAILED: &str = "kernel_module_build_failed";
pub const BUILD_KERNEL_MODULE_MISSING: &str = "kernel_module_missing";
pub const BUILD_KERNEL_MODULE_INSTALL_FAILED: &str = "kernel_module_install_failed";
pub const COMMUNITY_PATCH_IDS: [&str; 4] = ["0033", "0043", "0047", "0055"];

pub const BUILD_DOCKER_RUN_FAILED: &str = "docker_run_failed";
pub const BUILD_CONTAINER_EXIT: &str = "container_exit";
pub const BUILD_ARTIFACT_SHA_MISMATCH: &str = "artifact.sha_mismatch";
pub const BUILD_ARTIFACT_KERNEL_MISMATCH: &str = "artifact.kernel_mismatch";
pub const BUILD_ARTIFACT_KERNEL_AMBIGUOUS: &str = "artifact.kernel_ambiguous";
pub const BUILD_ARTIFACT_VERSION_MISMATCH: &str = "artifact.version_mismatch";
pub const BUILD_ARTIFACT_VERSION_UNREADABLE: &str = "artifact.version_unreadable";
pub const BUILD_ARTIFACT_TOOL_MISSING: &str = "artifact.tool_missing";
pub const BUILD_ARTIFACT_UNREADABLE: &str = "artifact.unreadable";
pub const BUILD_ARTIFACT_DELIVER_FAILED: &str = "artifact.deliver_failed";

pub const SUBMIT_RUNNER_COPY_FAILED: &str = "submit.runner_copy_failed";
pub const SUBMIT_STATE_UNWRITABLE: &str = "submit.state_unwritable";
pub const SUBMIT_TASK_DIR_FAILED: &str = "submit.task_dir_failed";
pub const SUBMIT_UNIT_NOT_CREATED: &str = "submit.unit_not_created";
pub const SUBMIT_EXEC_FAILED: &str = "submit.exec_failed";

pub const BUILD_STEP_NAMES: [&str; 14] = [
    "check",
    "modules",
    "compiler",
    "probe",
    "community_patch",
    "install",
    "install_userspace",
    "install_kernel_module",
    "depmod",
    "capture",
    "toolkit",
    "release",
    "package",
    "result",
];

/// Log line keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKey {
    PhaseEnter,
    Cmd,
    LoadedVersion,
    LoadedVersionNone,
    LoadedVersionUnknown,
    OriginalExists,
    OriginalSaved,
    BuildDeadlineSet,
    ToolkitSelected,
    FileDownloaded,
    BuildKitCache,
    BuildContainer,
    BuildStep,
    BuildCompiler,
    BuildInstallMode,
    BuildFlagUnsupported,
    BuildTimeout,
    ArtifactChecked,
    ArtifactCheckFailed,
    RowCreated,
    Occupancy,
    ReloadTargetDefault,
    ModuleUnloaded,
    ModuleUnloadRefused,
    RwMountConfirmed,
    TempVerified,
    DeadlineOk,
    DeadlineExceeded,
    WriteStopped,
    CanonicalWritten,
    Readback,
    ReadonlyRestored,
    Since,
    ModuleReloaded,
    ModuleReloadFailed,
    CheckItem,
    RestoreStart,
    RestoreNone,
    GpuUsers,
    Cleanup,
    CleanupFailed,
    Finished,
}

impl LogKey {
    pub fn as_str(self) -> &'static str {
        match self {
            LogKey::PhaseEnter => "log.phase_enter",
            LogKey::Cmd => "log.cmd",
            LogKey::LoadedVersion => "log.loaded_version",
            LogKey::LoadedVersionNone => "log.loaded_version_none",
            LogKey::LoadedVersionUnknown => "log.loaded_version_unknown",
            LogKey::OriginalExists => "log.original_exists",
            LogKey::OriginalSaved => "log.original_saved",
            LogKey::BuildDeadlineSet => "log.build_deadline_set",
            LogKey::ToolkitSelected => "log.toolkit_selected",
            LogKey::FileDownloaded => "log.file_downloaded",
            LogKey::BuildKitCache => "log.build_kit_cache",
            LogKey::BuildContainer => "log.build_container",
            LogKey::BuildStep => "log.build_step",
            LogKey::BuildCompiler => "log.build_compiler",
            LogKey::BuildInstallMode => "log.build_install_mode",
            LogKey::BuildFlagUnsupported => "log.build_flag_unsupported",
            LogKey::BuildTimeout => "log.build_timeout",
            LogKey::ArtifactChecked => "log.artifact_checked",
            LogKey::ArtifactCheckFailed => "log.artifact_check_failed",
            LogKey::RowCreated => "log.row_created",
            LogKey::Occupancy => "log.occupancy",
            LogKey::ReloadTargetDefault => "log.reload_target_default",
            LogKey::ModuleUnloaded => "log.module_unloaded",
            LogKey::ModuleUnloadRefused => "log.module_unload_refused",
            LogKey::RwMountConfirmed => "log.rw_mount_confirmed",
            LogKey::TempVerified => "log.temp_verified",
            LogKey::DeadlineOk => "log.deadline_ok",
            LogKey::DeadlineExceeded => "log.deadline_exceeded",
            LogKey::WriteStopped => "log.write_stopped",
            LogKey::CanonicalWritten => "log.canonical_written",
            LogKey::Readback => "log.readback",
            LogKey::ReadonlyRestored => "log.readonly_restored",
            LogKey::Since => "log.since",
            LogKey::ModuleReloaded => "log.module_reloaded",
            LogKey::ModuleReloadFailed => "log.module_reload_failed",
            LogKey::CheckItem => "log.check_item",
            LogKey::RestoreStart => "log.restore_start",
            LogKey::RestoreNone => "log.restore_none",
            LogKey::GpuUsers => "log.gpu_users",
            LogKey::Cleanup => "log.cleanup",
            LogKey::CleanupFailed => "log.cleanup_failed",
            LogKey::Finished => "log.finished",
        }
    }
}

/// `docker logs` events written by the web process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Starting,
    SettingUnknown,
    SettingInvalid,
    PortFallback,
    DataDirsFailed,
    StartupCheckFailed,
    StartupChecksPassed,
    CredentialsCreated,
    CredentialsInitialPasswordUnusable,
    CredentialsUnreadable,
    CredentialsUnsaved,
    Listening,
    ListenFailed,
    LoginSucceeded,
    LoginFailed,
    LoginRateLimited,
    Logout,
    PasswordChanged,
    CatalogQueryFailed,
    KernelHeadersUnreadable,
    GpusUnreadable,
    RunnerCopyFailed,
    TaskSubmitted,
    TaskSubmitFailed,
    TaskInterrupted,
    TaskReleased,
    StateFileKept,
    TaskCleanup,
    HostReset,
    TaskCleanupStepFailed,
    UnitQueryFailed,
    UploadRejected,
    DriversDeleted,
    SshLoginSucceeded,
    SshLoginFailed,
    SshKeyAdded,
    SshKeyRemoved,
    AdmissionBlocked,
}

impl Event {
    pub fn as_str(self) -> &'static str {
        match self {
            Event::Starting => "starting",
            Event::SettingUnknown => "setting_unknown",
            Event::SettingInvalid => "setting_invalid",
            Event::PortFallback => "port_fallback",
            Event::DataDirsFailed => "data_dirs_failed",
            Event::StartupCheckFailed => "startup_check_failed",
            Event::StartupChecksPassed => "startup_checks_passed",
            Event::CredentialsCreated => "credentials_created",
            Event::CredentialsInitialPasswordUnusable => "credentials_initial_password_unusable",
            Event::CredentialsUnreadable => "credentials_unreadable",
            Event::CredentialsUnsaved => "credentials_unsaved",
            Event::Listening => "listening",
            Event::ListenFailed => "listen_failed",
            Event::LoginSucceeded => "login_succeeded",
            Event::LoginFailed => "login_failed",
            Event::LoginRateLimited => "login_rate_limited",
            Event::Logout => "logout",
            Event::PasswordChanged => "password_changed",
            Event::CatalogQueryFailed => "catalog_query_failed",
            Event::KernelHeadersUnreadable => "kernel_headers_unreadable",
            Event::GpusUnreadable => "gpus_unreadable",
            Event::RunnerCopyFailed => "runner_copy_failed",
            Event::TaskSubmitted => "task_submitted",
            Event::TaskSubmitFailed => "task_submit_failed",
            Event::TaskInterrupted => "task_interrupted",
            Event::TaskReleased => "task_released",
            Event::StateFileKept => "state_file_kept",
            Event::TaskCleanup => "task_cleanup",
            Event::HostReset => "host_reset",
            Event::TaskCleanupStepFailed => "task_cleanup_step_failed",
            Event::UnitQueryFailed => "unit_query_failed",
            Event::UploadRejected => "upload_rejected",
            Event::DriversDeleted => "drivers_deleted",
            Event::SshLoginSucceeded => "ssh_login_succeeded",
            Event::SshLoginFailed => "ssh_login_failed",
            Event::AdmissionBlocked => "admission_blocked",
            Event::SshKeyAdded => "ssh_key_added",
            Event::SshKeyRemoved => "ssh_key_removed",
        }
    }

    pub fn msg(self) -> &'static str {
        match self {
            Event::Starting => "starting truenas-nvidia-driver-manager",
            Event::SettingUnknown => "unrecognized setting",
            Event::SettingInvalid => "invalid setting value",
            Event::PortFallback => "NVM_PORT is invalid; listening on the default port",
            Event::DataDirsFailed => "could not create data directories",
            Event::StartupCheckFailed => "startup check did not pass",
            Event::StartupChecksPassed => "startup checks passed",
            Event::CredentialsCreated => "administrator account created from the initial password",
            Event::CredentialsInitialPasswordUnusable => {
                "credentials file missing and the initial password is not usable"
            }
            Event::CredentialsUnreadable => "credentials file could not be read",
            Event::CredentialsUnsaved => "credentials file could not be saved",
            Event::Listening => "listening",
            Event::ListenFailed => "could not listen on the port",
            Event::LoginSucceeded => "sign-in succeeded",
            Event::LoginFailed => "sign-in failed",
            Event::LoginRateLimited => "sign-in rate limited",
            Event::Logout => "signed out",
            Event::PasswordChanged => "administrator password changed; other sessions signed out",
            Event::CatalogQueryFailed => "NVIDIA version index query failed",
            Event::KernelHeadersUnreadable => {
                "kernel headers could not be read; buildable branches cannot be determined"
            }
            Event::GpusUnreadable => {
                "host GPUs could not be read; versions cannot be matched to the GPUs"
            }
            Event::RunnerCopyFailed => "could not copy the task program to the data directory",
            Event::TaskSubmitted => "task submitted",
            Event::TaskSubmitFailed => "task could not be started",
            Event::TaskInterrupted => "task recorded as INTERRUPTED",
            Event::TaskReleased => "recovery material released after a later verified task",
            Event::StateFileKept => "unreadable task state file kept aside",
            Event::TaskCleanup => "task cleanup finished",
            Event::HostReset => "host reset step applied",
            Event::TaskCleanupStepFailed => "task cleanup step failed",
            Event::UnitQueryFailed => "host task unit query failed",
            Event::UploadRejected => "upload rejected",
            Event::DriversDeleted => "local drivers deleted",
            Event::SshLoginSucceeded => "SSH sign-in succeeded",
            Event::SshLoginFailed => "SSH sign-in failed",
            Event::AdmissionBlocked => "deployment admission blocked an operation",
            Event::SshKeyAdded => "SSH sign-in key registered",
            Event::SshKeyRemoved => "SSH sign-in key removed",
        }
    }

    /// Stable step names reported by `task_cleanup_step_failed` and `host_reset`.
    pub fn cleanup_step_code(step: CleanupStep) -> &'static str {
        match step {
            CleanupStep::DeleteTempFile => "delete_temp_file",
            CleanupStep::UnmountWritableMount => "unmount_writable_mount",
            CleanupStep::RestoreReadonly => "restore_readonly",
            CleanupStep::RemoveBuildContainer => "remove_build_container",
            CleanupStep::DeleteStateTemps => "delete_state_temps",
            CleanupStep::DeleteTaskDir => "delete_task_dir",
        }
    }
}

/// Cleanup actions; temporary-file deletion and unmounting are reported separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupStep {
    DeleteTempFile,
    UnmountWritableMount,
    RestoreReadonly,
    RemoveBuildContainer,
    DeleteStateTemps,
    DeleteTaskDir,
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const SHORT_NAME: &str = "nvm";
