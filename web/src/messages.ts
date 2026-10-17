// Every key has Chinese and English text with the same English placeholder names.
// Messages contain newline-separated segments. A line may start with a level
// (info, warning, error) and a kind (title, body, action, list, note), each
// followed by |. Untagged lines are body text; segments keep first-use order.
// Inline markup: backticks create code, ** creates strong emphasis,
// [[text|url]] creates links, and {name} inserts a parameter.
// Variants resolve key.variant first, then the base key.

export type Lang = "zh" | "en";

export interface Message {
  zh: string;
  en: string;
}

export const MESSAGES: Record<string, Message> = {
  "task.release.button": { zh: "释放恢复材料", en: "Release recovery material" },
  "task.release.confirm_title": { zh: "释放此任务的恢复材料？", en: "Release this task’s recovery material?" },
  "task.release.confirm_body": { zh: "程序会先核对更晚的成功任务、当前驱动和主机状态。只有确认安全后，才会删除此任务目录中的下载、构建输入与产物。任务记录和日志会保留。", en: "The program will first verify a later successful task, the current driver and the host state. Only after safety is confirmed will it delete downloads, build inputs and outputs in this task’s directory. Task records and logs will be kept." },
  "task.release.done": { zh: "info|恢复材料已于 {time} 由任务 `{task_id}` 释放。", en: "info|Recovery material was released at {time} by task `{task_id}`." },
  "task.release.pending": { zh: "warning|任务 `{task_id}` 已于 {time} 记录释放，目录清理仍待完成。可以再次点击“释放恢复材料”重试。", en: "warning|Task `{task_id}` recorded the release at {time}; directory cleanup is still pending. Choose “Release recovery material” again to retry." },
  "release.task_running": { zh: "error|无法释放恢复材料：有任务正在运行。请等待任务结束后重试。", en: "error|Recovery material cannot be released while a task is running. Wait for it to finish and retry." },
  "release.lock_busy": { zh: "error|无法释放恢复材料：另一个操作正在进行。请稍后重试。", en: "error|Recovery material cannot be released because another operation is in progress. Retry shortly." },
  "release.state_unknown": { zh: "error|无法释放恢复材料：无法确认任务或主机单元状态。请恢复查询后重试。", en: "error|Recovery material cannot be released because the task or host unit state could not be confirmed. Restore the query and retry." },
  "release.record_unreadable": { zh: "error|无法释放恢复材料：任务记录不可读或不可信。请保留材料并检查任务记录。", en: "error|Recovery material cannot be released because the task record is unreadable or untrusted. Keep the material and inspect the task record." },
  "release.recovery_pending": { zh: "error|无法释放恢复材料：仍有待恢复副本或不可读记录的保留文件。请先处理这些记录。", en: "error|Recovery material cannot be released while recovery copies or preserved unreadable records remain. Resolve those records first." },
  "release.not_terminal": { zh: "error|无法释放恢复材料：此任务尚未结束。请等待任务结束后重试。", en: "error|Recovery material cannot be released because this task has not finished. Wait for it to finish and retry." },
  "release.no_later_success": { zh: "error|无法释放恢复材料：没有更晚创建且已完成驱动替换核对的成功任务。请先完成一次成功的更新或恢复。", en: "error|Recovery material cannot be released because no later-created successful task has verified a driver replacement. Complete a successful update or restore first." },
  "release.driver_mismatch": { zh: "error|无法释放恢复材料：当前驱动、模块或驱动文件与更晚的成功任务记录不符，或无法核对。请先确认驱动状态。", en: "error|Recovery material cannot be released because the current driver, modules or driver files do not match a later successful task, or could not be verified. Check the driver state first." },
  "release.host_residue": { zh: "error|无法释放恢复材料：主机上仍有本工具留下的可写挂载，或只读属性未恢复。请先完成主机复位并核对。", en: "error|Recovery material cannot be released because writable mounts from this tool remain or read-only properties have not been restored. Complete and verify host reset first." },
  "release.write_failed": { zh: "error|无法记录恢复材料的释放。材料仍保留，请检查数据目录是否可写后重试。", en: "error|The recovery release could not be recorded. The material is still kept. Check that the data directory is writable and retry." },
  "release.cleanup_failed": { zh: "error|释放已记录，但恢复材料目录未能完全删除。任务记录和日志仍保留，请稍后重试清理。", en: "error|The release was recorded, but the recovery material directory could not be fully removed. Task records and logs are kept. Retry cleanup shortly." },


  "level.info": { zh: "信息", en: "Info" },
  "level.warning": { zh: "警告", en: "Warning" },
  "level.error": { zh: "错误", en: "Error" },

  "code.explanation": { zh: "error|**`{code}`** {code}", en: "error|**`{code}`** {code}" },
  "code.DOWNLOAD_FAILED": {
    zh: "error|**`DOWNLOAD_FAILED`** 下载失败。驱动文件未改动。",
    en: "error|**`DOWNLOAD_FAILED`** The download failed. The driver file was not changed.",
  },
  "code.BUILD_FAILED": {
    zh: "error|**`BUILD_FAILED`** 构建失败。驱动文件未改动。",
    en: "error|**`BUILD_FAILED`** The build failed. The driver file was not changed.",
  },
  "code.BUILD_TIMEOUT": {
    zh: "error|**`BUILD_TIMEOUT`** 任务超过 `NVM_BUILD_TIMEOUT_MINUTES` 的时限,已停止。驱动文件未改动。",
    en: "error|**`BUILD_TIMEOUT`** The task exceeded the `NVM_BUILD_TIMEOUT_MINUTES` time limit and was stopped. The driver file was not changed.",
  },
  "code.SAVE_ORIGINAL_FAILED": {
    zh: "error|**`SAVE_ORIGINAL_FAILED`** 保存原版驱动失败:{save_reason}。任务已停止,驱动文件未改动。",
    en: "error|**`SAVE_ORIGINAL_FAILED`** Saving the original driver failed: {save_reason}. The task stopped; the driver file was not changed.",
  },
  "code.WRITE_FAILED": {
    zh: "error|**`WRITE_FAILED`** 写入驱动文件失败:{write_reason}。驱动文件未改动。已重新加载已卸载的驱动模块,当前驱动仍为 {old}。",
    en: "error|**`WRITE_FAILED`** Writing the driver file failed: {write_reason}. The driver file was not changed. The driver modules already unloaded have been reloaded; the driver is still {old}.",
  },
  "code.WRITE_FAILED.reload_failed": {
    zh: "error|**`WRITE_FAILED`** 写入驱动文件失败:{write_reason}。驱动文件未改动。重新加载驱动模块失败:{reload_reason}。已卸载的驱动模块没有全部重新加载,申请了显卡的应用可能无法启动。",
    en: "error|**`WRITE_FAILED`** Writing the driver file failed: {write_reason}. The driver file was not changed. Reloading the driver modules failed: {reload_reason}. Not all of the unloaded driver modules were reloaded; applications that requested the GPU may fail to start.",
  },
  "code.REPLACE_DEADLINE_EXCEEDED": {
    zh: "error|**`REPLACE_DEADLINE_EXCEEDED`** 写入驱动文件之前已超过执行期限({deadline} 分钟),更新已停止。驱动文件未改动。已重新加载已卸载的驱动模块,当前驱动仍为 {old}。",
    en: "error|**`REPLACE_DEADLINE_EXCEEDED`** The execution deadline ({deadline} minutes) passed before the driver file was written, so the update stopped. The driver file was not changed. The driver modules already unloaded have been reloaded; the driver is still {old}.",
  },
  "code.REPLACE_DEADLINE_EXCEEDED.reload_failed": {
    zh: "error|**`REPLACE_DEADLINE_EXCEEDED`** 写入驱动文件之前已超过执行期限({deadline} 分钟),更新已停止。驱动文件未改动。重新加载驱动模块失败:{reload_reason}。已卸载的驱动模块没有全部重新加载,申请了显卡的应用可能无法启动。",
    en: "error|**`REPLACE_DEADLINE_EXCEEDED`** The execution deadline ({deadline} minutes) passed before the driver file was written, so the update stopped. The driver file was not changed. Reloading the driver modules failed: {reload_reason}. Not all of the unloaded driver modules were reloaded; applications that requested the GPU may fail to start.",
  },
  // Omit the reloaded-module sentence when no module was unloaded.
  "code.WRITE_FAILED.not_unloaded": {
    zh: "error|**`WRITE_FAILED`** 写入驱动文件失败:{write_reason}。驱动文件未改动。当前驱动仍为 {old}。",
    en: "error|**`WRITE_FAILED`** Writing the driver file failed: {write_reason}. The driver file was not changed. The driver is still {old}.",
  },
  "code.REPLACE_DEADLINE_EXCEEDED.not_unloaded": {
    zh: "error|**`REPLACE_DEADLINE_EXCEEDED`** 写入驱动文件之前已超过执行期限({deadline} 分钟),更新已停止。驱动文件未改动。当前驱动仍为 {old}。",
    en: "error|**`REPLACE_DEADLINE_EXCEEDED`** The execution deadline ({deadline} minutes) passed before the driver file was written, so the update stopped. The driver file was not changed. The driver is still {old}.",
  },
  "task.error_reason": { zh: "失败原因:{reason}。", en: "Reason: {reason}." },

  "reason.file_missing": { zh: "文件不存在", en: "the file does not exist" },
  "reason.permission_denied": { zh: "没有读取权限", en: "permission was denied" },
  "reason.io_error": { zh: "读取出错", en: "a read error occurred" },
  "reason.format_unrecognized": {
    zh: "内容格式无法识别",
    en: "the content format is not recognized",
  },
  "reason.record_unreadable": {
    zh: "本地驱动表的记录无法读取",
    en: "the record in Local drivers could not be read",
  },
  "reason.timed_out": {
    zh: "在 {seconds} 秒内没有完成",
    en: "it did not finish within {seconds} seconds",
  },
  "reason.exit_status": {
    zh: "命令以退出码 {code} 结束",
    en: "the command ended with exit code {code}",
  },
  "reason.signal": {
    zh: "命令被信号 {signal} 终止",
    en: "the command was terminated by signal {signal}",
  },
  "reason.spawn_failed": { zh: "命令无法启动", en: "the command could not be started" },
  "reason.connect_failed": {
    zh: "无法连接到服务器",
    en: "the connection to the server failed",
  },
  "reason.http_status": {
    zh: "服务器返回 HTTP {status}",
    en: "the server returned HTTP {status}",
  },
  "reason.index_unrecognized": {
    zh: "目录内容无法识别",
    en: "the index content is not recognized",
  },
  "reason.startup_check_failed": {
    zh: "启动检查未通过,无法读取主机",
    en: "the startup checks did not pass, so the host cannot be read",
  },

  "host.rw_mount_leftover": {
    zh: "上一次更新留下的可写挂载还在 {mount}",
    en: "the writable mount the last update left behind is still at {mount}",
  },
  "host.readonly_leftover": {
    zh: "驱动目录 {dataset} 的只读属性读回为 {reading},不是记录里的 {expected}",
    en: "the read-only property of {dataset} reads back as {reading}, not the recorded {expected}",
  },

  "admission.lock_unsupported": {
    zh: "主机不支持所需的文件锁",
    en: "the host does not support the required file lock",
  },
  "admission.lock_dir_invalid": {
    zh: "锁目录不可用",
    en: "the lock directory is not usable",
  },
  "admission.lock_busy": {
    zh: "另一个操作正在占用主机,请稍后重试",
    en: "another operation is using the host; retry in a moment",
  },
  "admission.lock_timeout": {
    zh: "等待主机空闲超时",
    en: "waiting for the host to become free timed out",
  },
  "drain.pending_job": {
    zh: "任务单元 {unit} 还有排队的作业",
    en: "the task unit {unit} still has a queued job",
  },
  "drain.state_scan_failed": {
    zh: "状态目录里有无法解释的文件 {name}",
    en: "the state directory holds a file no rule explains: {name}",
  },
  "drain.unexplained_unit_row": {
    zh: "无法解释的任务单元行:{row}",
    en: "a task unit row this page cannot explain: {row}",
  },

  "save.canonical_unreadable": {
    zh: "无法读取正式路径上的驱动文件",
    en: "the driver file at the permanent path could not be read",
  },
  "save.write_failed": {
    zh: "无法在数据目录中写入或回读保存的原版驱动",
    en: "the saved original driver could not be written to or read back from the data directory",
  },
  "save.readback_mismatch": {
    zh: "保存后回读的校验值与原文件不符",
    en: "the checksum read back after saving does not match the original file",
  },
  "save.facts_unreadable": {
    zh: "无法从原版驱动文件读出驱动版本或目标内核",
    en: "the driver version or target kernel could not be read from the original driver file",
  },

  "write.dataset_unknown": {
    zh: "无法确定 `/usr` 所在的数据集",
    en: "the dataset of `/usr` could not be determined",
  },
  "write.readonly_unreadable": {
    zh: "无法读取 `/usr` 所在数据集的只读属性",
    en: "the readonly property of the `/usr` dataset could not be read",
  },
  "write.rw_mount_not_confirmed": {
    zh: "无法确认底层数据集的可写挂载",
    en: "the writable mount of the underlying dataset could not be confirmed",
  },
  "write.source_unreadable": {
    zh: "无法读取要写入的驱动文件",
    en: "the driver file to write could not be read",
  },
  "write.temp_failed": { zh: "写入临时文件失败", en: "writing the temporary file failed" },
  "write.temp_mismatch": {
    zh: "临时文件的校验值或字节数与记录值不符",
    en: "the checksum or size of the temporary file does not match the recorded value",
  },
  "write.rename_failed": { zh: "`mv -T` 失败", en: "`mv -T` failed" },

  "check.item.refresh": {
    zh: "驱动扩展刷新",
    en: "driver extension refresh",
  },
  "check.item.version": { zh: "驱动版本", en: "driver version" },
  "check.reason": {
    zh: "{check_item}的观测值为 {observed},期望值为 {expected}",
    en: "{check_item} was {observed}; expected {expected}",
  },
  "check.observed.refresh_both": {
    zh: "刷新前 {before}、刷新后 {after}",
    en: "{before} before and {after} after the refresh",
  },
  "check.observed.refresh_unconfirmed": {
    zh: "未确认扩展已卸下并重新合并、刷新前 {before}、刷新后 {after}",
    en: "unmerge and subsequent merge not confirmed; {before} before and {after} after",
  },
  "check.observed.refresh_before_absent": {
    zh: "刷新前无 `/usr` 上的 nvidia 扩展",
    en: "no nvidia extension on `/usr` before the refresh",
  },
  "check.observed.refresh_before_unmerged": {
    zh: "更新开始时扩展未合并,无法比较前后合并时间(任务末尾会重新合并,重试即可)",
    en: "the extension was not merged when the update started, so the merge times cannot be compared (the task merges it again at the end; retry)",
  },
  "check.observed.refresh_after_absent": {
    zh: "刷新后无 `/usr` 上的 nvidia 扩展",
    en: "no nvidia extension on `/usr` after the refresh",
  },
  "check.observed.status_unreadable": {
    zh: "`systemd-sysext status` 无法读取",
    en: "`systemd-sysext status` unreadable",
  },
  "check.expected.refresh": {
    zh: "已确认扩展卸下并重新合并，且合并时间与之前不同",
    en: "a confirmed unmerge and subsequent merge, with a merge time different from before",
  },
  "check.observed.version_both": {
    zh: "`nvidia-smi` {smi_version}、`/proc/driver/nvidia/version` {proc_version}",
    en: "`nvidia-smi` {smi_version}, `/proc/driver/nvidia/version` {proc_version}",
  },
  "check.observed.version_unreadable": { zh: "无法读取", en: "unreadable" },
  "check.expected.version": { zh: "{target}", en: "{target}" },
  "check.pass": { zh: "通过", en: "passed" },
  "check.fail": { zh: "未通过", en: "failed" },

  "build.run_check_failed": {
    zh: "安装包完整性检查(`--check`)未通过",
    en: "the installer integrity check (`--check`) failed",
  },
  "build.modules_copy_failed": {
    zh: "无法准备当前内核的模块目录",
    en: "the module directory of the current kernel could not be prepared",
  },
  "build.kernel_gcc_unknown": {
    zh: "无法确定当前内核所用的编译器版本",
    en: "the compiler version used for the current kernel could not be determined",
  },
  "build.compiler_missing": {
    zh: "编译环境里没有当前内核所用的编译器 gcc-{version}",
    en: "the build environment does not include gcc-{version}, the compiler used for the current kernel",
  },
  "build.extract_failed": { zh: "无法解开安装包", en: "the installer could not be extracted" },
  "build.community_patch_failed.archive": {
    zh: "社区补丁或源码校验未通过",
    en: "community patch or source verification failed",
  },
  "build.community_patch_failed": {
    zh: "应用社区补丁 {patch} 失败",
    en: "applying community patch {patch} failed",
  },
  "build.options_unreadable": {
    zh: "无法读取安装程序支持的参数",
    en: "the options supported by the installer could not be read",
  },
  "build.kernel_module_type_unsupported": {
    zh: "这个安装包不能按所选显卡指定内核模块类型(缺少 `--kernel-module-type`),请换用保留了这个参数的安装包",
    en: "this installer cannot set the kernel module type for the selected GPUs (`--kernel-module-type` is missing); use an installer that keeps this option",
  },
  "build.installer_failed": { zh: "NVIDIA 安装程序失败", en: "the NVIDIA installer failed" },
  "build.installer_userspace_failed": {
    zh: "NVIDIA 安装程序在安装用户态文件时失败",
    en: "the NVIDIA installer failed while installing the user-space files",
  },
  "build.installer_kernel_module_failed": {
    zh: "NVIDIA 安装程序在构建内核模块时失败",
    en: "the NVIDIA installer failed while building the kernel modules",
  },
  "build.kernel_module_build_failed": {
    zh: "内核模块编译失败",
    en: "kernel module compilation failed",
  },
  "build.kernel_module_missing": {
    zh: "编译后的内核模块 {module}.ko 缺失、为空或不是普通文件",
    en: "the compiled kernel module {module}.ko is missing, empty, or not a regular file",
  },
  "build.kernel_module_install_failed": {
    zh: "未能安装编译后的内核模块 {module}.ko",
    en: "the compiled kernel module {module}.ko could not be installed",
  },
  "build.depmod_failed": {
    zh: "生成模块索引失败",
    en: "generating the module indexes failed",
  },
  "build.capture_failed": {
    zh: "收取安装程序写下的文件失败",
    en: "collecting the files written by the installer failed",
  },
  "build.deb_extract_failed": {
    zh: "解开 toolkit 包 {package} 失败",
    en: "extracting the toolkit package {package} failed",
  },
  "build.release_write_failed": {
    zh: "写入扩展标识失败",
    en: "writing the extension identity failed",
  },
  "build.mksquashfs_failed": { zh: "打包驱动镜像失败", en: "packaging the driver image failed" },
  "build.unexpected": {
    zh: "构建脚本在第 {line} 行意外停止",
    en: "the build script stopped unexpectedly at line {line}",
  },

  "build.docker_run_failed": {
    zh: "无法启动构建容器",
    en: "the build container could not be started",
  },
  "build.container_exit": {
    zh: "构建容器以退出码 {code} 结束",
    en: "the build container ended with exit code {code}",
  },
  "build.artifact.sha_mismatch": {
    zh: "构建出的驱动校验值与构建记录不符",
    en: "the checksum of the built driver does not match the build record",
  },
  "build.artifact.kernel_mismatch": {
    zh: "构建出的驱动目标内核为 {built_kernel},当前内核为 {current_kernel}",
    en: "the built driver targets kernel {built_kernel}; the current kernel is {current_kernel}",
  },
  "build.artifact.kernel_ambiguous": {
    zh: "构建出的驱动里没有内核目录,或有不止一个",
    en: "the built driver has no kernel directory, or has more than one",
  },
  "build.artifact.version_mismatch": {
    zh: "构建出的驱动版本为 {built_version},目标版本为 {target}",
    en: "the built driver is version {built_version}; the target version is {target}",
  },
  "build.artifact.version_unreadable": {
    zh: "无法读取构建出的驱动版本",
    en: "the driver version of the built driver could not be read",
  },
  "build.artifact.tool_missing": {
    zh: "构建出的驱动里没有可执行的 {program}",
    en: "the built driver has no executable {program}",
  },
  "build.artifact.unreadable": {
    zh: "无法读取构建出的驱动内容",
    en: "the content of the built driver could not be read",
  },
  "build.artifact.deliver_failed": {
    zh: "无法把构建出的驱动写入数据目录",
    en: "the built driver could not be written to the data directory",
  },

  "download.request_failed": {
    zh: "下载 {file_name} 失败",
    en: "downloading {file_name} failed",
  },
  "download.http_status": {
    zh: "下载 {file_name} 时服务器返回 HTTP {status}",
    en: "the server returned HTTP {status} for {file_name}",
  },
  "download.toolkit_index_unreadable": {
    zh: "无法读取 toolkit 源的索引",
    en: "the toolkit repository index could not be read",
  },
  "download.toolkit_no_common_version": {
    zh: "toolkit 源里没有 4 个包都有的版本",
    en: "the toolkit repository has no version that all four packages share",
  },
  "download.toolkit_too_old": {
    zh: "toolkit 源里 4 个包都有的最新版本 {toolkit_version} 低于驱动 {driver_version} 所需的 1.20.1",
    en: "the newest toolkit version {toolkit_version} shared by all four packages in the toolkit repository is below the 1.20.1 required by driver {driver_version}",
  },
  "download.toolkit_entry_unexpected": {
    zh: "toolkit 源索引里 {package} 的文件名不符合预期",
    en: "the file name of {package} in the toolkit repository index is not as expected",
  },
  "download.checksum_mismatch": {
    zh: "{file_name} 的大小或校验值与预期值不符",
    en: "the size or checksum of {file_name} does not match the expected value",
  },

  "branch.follow_pfn_removed": {
    zh: "内核里已没有驱动源码调用的函数 `follow_pfn()`",
    en: "the kernel no longer has the function `follow_pfn()` that the driver source calls",
  },
  "branch.output_poll_changed_removed": {
    zh: "内核的 `drm_mode_config_funcs` 已没有驱动源码使用的成员 `output_poll_changed`",
    en: "the kernel's `drm_mode_config_funcs` no longer has the member `output_poll_changed` that the driver source uses",
  },
  "branch.pci_resize_resource_args": {
    zh: "内核的 `pci_resize_resource()` 参数个数与这个分支驱动源码里的调用不一致",
    en: "the number of arguments of the kernel's `pci_resize_resource()` does not match the call in this branch's driver source",
  },
  "branch.installer_loads_module": {
    zh: "这个分支的安装程序在构建时会试着加载内核模块,而构建容器里不能加载模块",
    en: "this branch's installer tries to load the kernel module during the build, which the build container does not allow",
  },
  "branch.nvidia_not_loaded": {
    zh: "主机上没有加载 `nvidia` 模块,所以这个分支的安装程序在构建时会试着加载内核模块,而构建容器里不能加载模块",
    en: "the `nvidia` module is not loaded on the host, so this branch's installer tries to load the kernel module during the build, which the build container does not allow",
  },
  "branch.nvidia_drm_not_loaded": {
    zh: "主机上没有加载 `nvidia_drm` 模块,所以这个分支的安装程序装完后要去加载它,而构建容器里不能加载模块",
    en: "the `nvidia_drm` module is not loaded on the host, so this branch's installer has to load it after installing, which the build container does not allow",
  },

  "restore.no_original.not_saved": {
    zh: "本次更新未保存原版驱动",
    en: "no original driver was saved for this update",
  },
  "restore.no_original.checksum_mismatch": {
    zh: "原版驱动文件校验不符",
    en: "the original driver file failed verification",
  },
  "restore.no_original.unreadable": {
    zh: "无法读取原版驱动文件:{read_reason}",
    en: "the original driver file could not be read: {read_reason}",
  },
  "state_read_error.reason.missing": { zh: "文件不存在", en: "the file does not exist" },
  "state_read_error.reason.incomplete": { zh: "内容不完整", en: "the content is incomplete" },
  "state_read_error.reason.invalid": { zh: "内容不合法", en: "the content is invalid" },
  "state_read_error.reason.permission_denied": { zh: "没有读取权限", en: "permission was denied" },
  "state_read_error.reason.io_error": { zh: "读取出错", en: "a read error occurred" },

  "status.SUCCEEDED": { zh: "成功", en: "Succeeded" },
  "status.STOCK_RESTORED": { zh: "已恢复原版驱动", en: "Original driver restored" },
  "status.FAILED": { zh: "失败", en: "Failed" },
  "status.INTERRUPTED": { zh: "已中断", en: "Interrupted" },
  "status.RUNNING": { zh: "未结束", en: "Not finished" },
  "status.UNKNOWN": { zh: "UNKNOWN", en: "UNKNOWN" },

  "phase.read_loaded_version": {
    zh: "读取已加载的驱动版本",
    en: "Reading the loaded driver version",
  },
  "phase.save_original": { zh: "保存原版驱动", en: "Saving the original driver" },
  "phase.download": { zh: "下载", en: "Downloading" },
  "phase.build": { zh: "构建", en: "Building" },
  "phase.unload": { zh: "卸载驱动模块", en: "Unloading driver modules" },
  "phase.write": { zh: "写入驱动文件", en: "Writing the driver file" },
  "phase.refresh": { zh: "刷新驱动扩展", en: "Refreshing the driver extension" },
  "phase.reload": { zh: "重新加载驱动模块", en: "Reloading driver modules" },
  "phase.check": { zh: "成功检查", en: "Post-update check" },
  "phase.restore_unload": {
    zh: "恢复原版驱动 · 卸载驱动模块",
    en: "Restoring the original driver · Unloading driver modules",
  },
  "phase.restore_write": {
    zh: "恢复原版驱动 · 写入驱动文件",
    en: "Restoring the original driver · Writing the driver file",
  },
  "phase.restore_refresh": {
    zh: "恢复原版驱动 · 刷新驱动扩展",
    en: "Restoring the original driver · Refreshing the driver extension",
  },
  "phase.restore_reload": {
    zh: "恢复原版驱动 · 重新加载驱动模块",
    en: "Restoring the original driver · Reloading driver modules",
  },
  "phase.restore_check": {
    zh: "恢复原版驱动 · 成功检查",
    en: "Restoring the original driver · Post-update check",
  },
  "phase.read_loaded_version_end": {
    zh: "再次读取已加载的驱动版本",
    en: "Reading the loaded driver version again",
  },

  "home.interrupted": {
    zh: "warning|{time} 开始从 {source} 更新到 {version}，停在{phase}。发现中断的时间是 {discovered}。这一步在写入驱动文件之时或之后，页面无法判断文件是否已经换过。",
    en: "warning|The update started at {time} from {source}, toward {version}, and stopped during {phase}. The interruption was discovered at {discovered}. This step is at or after the driver file is written, so this page cannot tell whether the file was replaced.",
  },
  "home.interrupted.file_kept": {
    zh: "warning|{time} 开始从 {source} 更新到 {version}，停在{phase}。发现中断的时间是 {discovered}。正式路径上的驱动文件没有被替换。",
    en: "warning|The update started at {time} from {source}, toward {version}, and stopped during {phase}. The interruption was discovered at {discovered}. The driver file on the permanent path was not replaced.",
  },
  "home.interrupted.unload": {
    zh: "warning|{time} 开始从 {source} 更新到 {version}，停在{phase}。发现中断的时间是 {discovered}。正式路径上的驱动文件没有被替换。驱动模块可能已经卸载。请先看当前状态,再按情况重新选择版本更新,或重启主机后确认应用能使用显卡。",
    en: "warning|The update started at {time} from {source}, toward {version}, and stopped during {phase}. The interruption was discovered at {discovered}. The driver file on the permanent path was not replaced. Driver modules may already be unloaded. Check the current state first, then choose a version and update again, or restart the host and confirm applications can use the GPU.",
  },
  "home.interrupted.modules": {
    zh: "warning|{time} 开始从 {source} 更新到 {version}，停在{phase}。发现中断的时间是 {discovered}。这一步在写入驱动文件之时或之后，页面无法判断文件是否已经换过。驱动模块可能已卸载且没有装回。请先看当前状态,再按情况重新选择版本更新,或重启主机后确认应用能使用显卡。",
    en: "warning|The update started at {time} from {source}, toward {version}, and stopped during {phase}. The interruption was discovered at {discovered}. This step is at or after the driver file is written, so this page cannot tell whether the file was replaced. Driver modules may be unloaded without having been loaded again. Check the current state first, then choose a version and update again, or restart the host and confirm applications can use the GPU.",
  },
  "home.interrupted.phase_unknown": {
    zh: "warning|{time} 开始从 {source} 更新到 {version}。停在哪一步没有记下。发现中断的时间是 {discovered}。页面无法判断驱动文件是否已经换过。",
    en: "warning|The update started at {time} from {source}, toward {version}. The step it stopped on was not recorded. The interruption was discovered at {discovered}. This page cannot tell whether the driver file was replaced.",
  },
  "home.interrupted.next": { zh: "重新选择版本", en: "Choose a version again" },
  "home.interrupted.details": { zh: "详情", en: "Details" },
  "home.interrupted.task": { zh: "任务编号 {task_id}。", en: "Task {task_id}." },
  "home.interrupted.basis": {
    zh: "开始时间来自任务记录。中断实际发生的时刻没有记下，发现时间不是中断时刻。",
    en: "The start time comes from the task record. The moment the interruption happened was not recorded; the discovery time is not that moment.",
  },
  "home.interrupted.artifact": {
    zh: "到[[本地驱动|#/drivers]]查看已保存的驱动。",
    en: "See saved drivers in [[Local drivers|#/drivers]].",
  },
  "home.latest.none": {
    zh: "还没有更新记录。",
    en: "No update has been recorded.",
  },
  "home.latest.record": {
    zh: "最近一次任务于 {time} 开始，状态是 {status}，目标版本 {version}，来源是 {source}。",
    en: "The latest task started at {time}. Its status is {status}, its target version is {version}, and its source is {source}.",
  },
  "home.gpus": { zh: "显卡", en: "GPUs" },
  "home.gpus.none": { zh: "没有 NVIDIA 显示设备。", en: "No NVIDIA display device." },
  "home.gpus.unlisted": { zh: "表中没有 {device}", en: "{device} is not in the table" },
  "home.gpus.passthrough": { zh: "直通", en: "Passthrough" },
  "home.gpus.meta": { zh: "{address} · {device}", en: "{address} · {device}" },
  "home.gpus.basis": {
    zh: "名称按设备 ID 查表。这里没有子系统标识。",
    en: "The name comes from the device-ID table. No subsystem id is available.",
  },
  "home.gpus.unknown": { zh: "显卡读数无法完成。", en: "The GPU reading could not be completed." },

  "log.phase_enter": { zh: "进入阶段:{phase}。", en: "Stage started: {phase}." },
  "log.cmd": {
    zh: "命令 `{command}` 结束:{exit}({ms} ms)。",
    en: "Command `{command}` finished: {exit} ({ms} ms).",
  },
  "log.exit.code": { zh: "退出码 {code}", en: "exit code {code}" },
  "log.exit.signal": { zh: "被信号 {signal} 终止", en: "killed by signal {signal}" },
  "log.exit.timed_out": { zh: "超时,已结束", en: "timed out and was stopped" },
  "log.exit.spawn_failed": { zh: "无法启动", en: "could not be started" },
  "log.loaded_version": { zh: "已加载的驱动版本:{version}。", en: "Loaded driver version: {version}." },
  "log.loaded_version_none": {
    zh: "没有加载 NVIDIA 驱动模块。",
    en: "No NVIDIA driver module is loaded.",
  },
  "log.loaded_version_unknown": {
    zh: "无法读取已加载的驱动版本:{read_reason}。",
    en: "The loaded driver version could not be read: {read_reason}.",
  },
  "log.original_exists": {
    zh: "当前内核与 TrueNAS 版本的原版驱动已存在,保持原样。",
    en: "The original driver for the current kernel and TrueNAS version already exists and was kept as is.",
  },
  "log.original_saved": {
    zh: "已保存原版驱动 {version}(目标内核 {target_kernel},校验值 {checksum})。",
    en: "Saved the original driver {version} (target kernel {target_kernel}, checksum {checksum}).",
  },
  "log.build_deadline_set": {
    zh: "下载与构建须在 {time} 之前完成(`NVM_BUILD_TIMEOUT_MINUTES` 为 {minutes})。",
    en: "Download and build must finish by {time} (`NVM_BUILD_TIMEOUT_MINUTES` is {minutes}).",
  },
  "log.toolkit_selected": { zh: "toolkit 选用版本 {version}。", en: "Selected toolkit version {version}." },
  "log.file_downloaded": {
    zh: "已下载 {file_name}({bytes} 字节)。",
    en: "Downloaded {file_name} ({bytes} bytes).",
  },
  "log.build_kit_cache": {
    zh: "编译包缓存没有用上:{reason}。编译继续。",
    en: "The build-package cache was not used: {reason}. The build continues.",
  },
  "log.build_container": {
    zh: "构建容器 {container} 已启动。",
    en: "Build container {container} started.",
  },
  "log.build_step": { zh: "构建:{build_step}。", en: "Build: {build_step}." },
  "log.build_compiler": { zh: "使用编译器 {compiler}。", en: "Using compiler {compiler}." },
  "log.build_install_mode": {
    zh: "安装方式:{install_mode}。",
    en: "Installation: {install_mode}.",
  },
  "log.install_mode.single": { zh: "一遍", en: "single pass" },
  "log.install_mode.two_pass": {
    zh: "两遍(先用户态,后内核模块)",
    en: "two passes (user space first, then kernel modules)",
  },
  "log.install_mode.userspace-make": {
    zh: "安装用户态文件，再编译并安装内核模块",
    en: "install user-space files, then compile and install the kernel modules",
  },
  "log.build_flag_unsupported": {
    zh: "安装程序不支持参数 `{flag}`,已省略。",
    en: "The installer does not support `{flag}`; it was left out.",
  },
  "log.build_timeout": {
    zh: "下载与构建已到时限,已停止。",
    en: "Download and build reached the time limit and were stopped.",
  },
  "log.artifact_checked": {
    zh: "交付前核对通过:校验值、目标内核 {target_kernel}、驱动版本 {version}、toolkit 的三个程序。",
    en: "Pre-delivery check passed: checksum, target kernel {target_kernel}, driver version {version}, the three toolkit programs.",
  },
  "log.artifact_check_failed": {
    zh: "交付前核对未通过:{build_reason}。",
    en: "Pre-delivery check failed: {build_reason}.",
  },
  "log.row_created": {
    zh: "本地驱动表新增一行:{version}({source})。",
    en: "Added a row to Local drivers: {version} ({source}).",
  },
  "log.occupancy": {
    zh: "显卡占用判定:{usage_result};驱动模块引用数:{module_readings}。",
    en: "GPU usage: {usage_result}; driver module references: {module_readings}.",
  },
  "log.usage.not_in_use": { zh: "未被占用", en: "not in use" },
  "log.usage.busy": { zh: "被占用", en: "in use" },
  "log.usage.unreadable": { zh: "无法读取", en: "unreadable" },
  "log.module_readings.entry": { zh: "{module} {refcount}/{dependents}", en: "{module} {refcount}/{dependents}" },
  "log.module_readings.none": { zh: "没有已加载的驱动模块", en: "no driver module is loaded" },
  "log.reload_target_default": {
    zh: "任务开始时没有加载驱动模块,装回时按 `nvidia`、`nvidia_drm`、`nvidia_modeset` 的顺序加载。",
    en: "No driver module was loaded when the task started; `nvidia`, `nvidia_drm` and `nvidia_modeset` will be loaded in that order.",
  },
  "log.module_unloaded": { zh: "已卸载驱动模块 {module}。", en: "Unloaded driver module {module}." },
  "log.module_unload_refused": {
    zh: "卸载驱动模块 {module} 被系统拒绝:{unload_reason}。",
    en: "Unloading driver module {module} was refused: {unload_reason}.",
  },
  "log.rw_mount_confirmed": {
    zh: "已确认数据集 {dataset} 的可写挂载(只读属性原值为 {value})。",
    en: "Confirmed the writable mount of dataset {dataset} (readonly was {value}).",
  },
  "log.temp_verified": {
    zh: "临时文件核对通过(校验值 {checksum},{bytes} 字节)。",
    en: "The temporary file matches (checksum {checksum}, {bytes} bytes).",
  },
  "log.deadline_ok": {
    zh: "执行期限剩余 {seconds} 秒,执行 `mv -T`。",
    en: "{seconds} seconds of the execution deadline remain; running `mv -T`.",
  },
  "log.deadline_exceeded": {
    zh: "执行期限({deadline} 分钟)已到,在 `mv -T` 之前停止。",
    en: "The execution deadline ({deadline} minutes) passed; stopped before `mv -T`.",
  },
  "log.write_stopped": {
    zh: "在 `mv -T` 之前停止:{write_reason}。正式路径上的文件未改动。",
    en: "Stopped before `mv -T`: {write_reason}. The file at the permanent path was not changed.",
  },
  "log.canonical_written": { zh: "已用 `mv -T` 写入正式路径。", en: "Wrote the permanent path with `mv -T`." },
  "log.readback": {
    zh: "正式路径整文件回读,校验值{readback_result}(只记录)。",
    en: "The permanent path was read back in full; the checksum {readback_result} (recorded only).",
  },
  "log.readback.match": { zh: "与写入的一致", en: "matches what was written" },
  "log.readback.mismatch": { zh: "与写入的不一致", en: "does not match what was written" },
  "log.readonly_restored": {
    zh: "已把数据集 {dataset} 的只读属性设回 {value};随后读到的值为 {reading}。",
    en: "Set readonly on dataset {dataset} back to {value}; it now reads {reading}.",
  },
  "log.reading.on": { zh: "`on`", en: "`on`" },
  "log.reading.off": { zh: "`off`", en: "`off`" },
  "log.reading.unreadable": { zh: "无法读取", en: "unreadable" },
  "log.since": {
    zh: "驱动扩展合并时间:刷新前 {before},刷新后 {after}。",
    en: "Driver extension merge time: {before} before the refresh, {after} after.",
  },
  "log.since.none": { zh: "无 nvidia 扩展", en: "no nvidia extension" },
  "log.module_reloaded": { zh: "已重新加载驱动模块 {module}。", en: "Reloaded driver module {module}." },
  "log.module_reload_failed": {
    zh: "重新加载驱动模块 {module} 失败:{raw}。",
    en: "Reloading driver module {module} failed: {raw}.",
  },
  "log.check_item": {
    zh: "成功检查 · {check_item}:{check_result}(观测值 {observed},期望值 {expected})。",
    en: "Post-update check · {check_item}: {check_result} (observed {observed}; expected {expected}).",
  },
  "log.restore_start": {
    zh: "开始恢复原版驱动 {original}(保存于 {date})。",
    en: "Restoring the original driver {original} (saved {date}).",
  },
  "log.restore_none": {
    zh: "不恢复原版驱动:{reason}。",
    en: "The original driver is not restored: {reason}.",
  },
  "log.gpu_users": {
    zh: "申请了显卡的应用:{applications}。",
    en: "Applications that requested the GPU: {applications}.",
  },
  "log.applications.entry": { zh: "{name} · {type}", en: "{name} · {type}" },
  "log.applications.none": { zh: "无", en: "none" },
  "log.applications.query_failed": { zh: "{type}查询失败", en: "{type} query failed" },
  "log.cleanup": {
    zh: "已清理本任务的临时文件与构建容器。",
    en: "Cleaned up this task's temporary files and build container.",
  },
  "log.cleanup_failed": {
    zh: "清理步骤「{cleanup_step}」失败:{reason}。",
    en: "Cleanup step \"{cleanup_step}\" failed: {reason}.",
  },
  "log.cleanup_step.delete_temp": { zh: "删除临时文件", en: "delete the temporary file" },
  "log.cleanup_step.unmount": { zh: "卸载可写挂载", en: "unmount the writable mount" },
  "log.cleanup_step.readonly": { zh: "设回只读属性", en: "restore the readonly property" },
  "log.cleanup_step.remove_container": { zh: "删除构建容器", en: "remove the build container" },
  "log.cleanup_step.state_tmp": {
    zh: "删除状态文件的临时文件",
    en: "delete the state file's temporary files",
  },
  "log.cleanup_step.task_dir": { zh: "删除任务目录", en: "delete the task directory" },
  "log.finished": { zh: "任务结束:{status}。", en: "Task finished: {status}." },
  "log.build_step.check": { zh: "检查安装包完整性", en: "checking the installer integrity" },
  "log.build_step.modules": { zh: "准备模块目录", en: "preparing the module directory" },
  "log.build_step.compiler": { zh: "选择编译器", en: "selecting the compiler" },
  "log.build_step.probe": {
    zh: "查询安装程序支持的参数",
    en: "reading the installer's supported options",
  },
  "log.build_step.install": { zh: "运行 NVIDIA 安装程序", en: "running the NVIDIA installer" },
  "log.build_step.install_userspace": { zh: "安装用户态文件", en: "installing the user-space files" },
  "log.build_step.community_patch": {
    zh: "应用社区补丁",
    en: "Apply community patches",
  },
  "log.build_step.install_kernel_module": {
    zh: "编译并安装内核模块",
    en: "Compile and install kernel modules",
  },
  "log.build_step.depmod": { zh: "生成模块索引", en: "generating the module indexes" },
  "log.build_step.capture": {
    zh: "收取安装程序写下的文件",
    en: "collecting the files written by the installer",
  },
  "log.build_step.toolkit": { zh: "解开 toolkit 包", en: "extracting the toolkit packages" },
  "log.build_step.release": { zh: "写入扩展标识", en: "writing the extension identity" },
  "log.build_step.package": { zh: "打包驱动镜像", en: "packaging the driver image" },
  "log.build_step.result": { zh: "记录构建结果", en: "recording the build result" },
  "list.object": { zh: "{name} · {type}", en: "{name} · {type}" },

  "login.failed": {
    zh: "error|用户名或密码不正确。",
    en: "error|The username or password is incorrect.",
  },
  "login.rate_limited": {
    zh: "error|连续登录失败次数过多,请在 {seconds} 秒后重试。",
    en: "error|Too many failed sign-in attempts. Try again in {seconds} seconds.",
  },
  "login.fields_required": {
    zh: "error|请输入用户名和密码。",
    en: "error|Enter the username and password.",
  },
  "logout.done": { zh: "已退出登录。", en: "You have signed out." },
  "logout.unsaved": {
    zh: "error|退出未能保存。本页已退出。请修改密码，使这次登录失效。",
    en: "error|Sign-out was not saved. This page is signed out. Change the password to end this session.",
  },

  "ssh.login.summary": {
    zh: "使用 SSH 密钥登录",
    en: "Sign in with an SSH key",
  },
  "ssh.login.step_challenge": {
    zh: "list|第 1 步:复制挑战字符串,在它过期前完成后面两步。",
    en: "list|Step 1: copy the challenge string and finish the next two steps before it expires.",
  },
  "ssh.login.step_sign": {
    zh: "list|第 2 步:在你的终端里运行命令签名;私钥不会进入本页面。",
    en: "list|Step 2: run the command in your terminal to sign it; the private key never enters this page.",
  },
  "ssh.login.command_file": {
    zh: "命令(私钥文件)",
    en: "Command (private key file)",
  },
  "ssh.login.command_agent": {
    zh: "命令(ssh-agent / 1Password,只有一把相关密钥)",
    en: "Command (ssh-agent / 1Password with one relevant key)",
  },
  "ssh.login.command_agent_multi": {
    zh: "命令(agent 有多把密钥:指向已登记公钥文件的路径)",
    en: "Command (agent with several keys: the registered public key file path)",
  },
  "ssh.login.command_pick": {
    zh: "note|不确定用哪把密钥?登录后在「账户」页复制每把密钥的现成命令。",
    en: "note|Not sure which key? After signing in, copy the ready-made command for each key on the Account page.",
  },
  "ssh.login.step_paste": {
    zh: "list|第 3 步:把签名全文粘贴到下面,点「登录」。",
    en: "list|Step 3: paste the whole signature below and press Sign in.",
  },
  "ssh.login.challenge_label": { zh: "挑战", en: "Challenge" },
  "ssh.login.expires": {
    zh: "挑战 2 分钟内有效。",
    en: "The challenge is valid for 2 minutes.",
  },
  "ssh.login.refresh": { zh: "重新获取挑战", en: "Get a new challenge" },
  "ssh.login.submit": { zh: "登录", en: "Sign in" },
  "ssh.login.signature_label": {
    zh: "签名(`-----BEGIN SSH SIGNATURE-----` 开头的全文)",
    en: "Signature (the full text starting with `-----BEGIN SSH SIGNATURE-----`)",
  },
  "ssh.login.pick_file": { zh: "选择 .sig 文件", en: "Choose a .sig file" },
  "ssh.login.copy": { zh: "复制", en: "Copy" },
  "ssh.login.copied": { zh: "已复制。", en: "Copied." },
  "ssh.challenge_invalid": {
    zh: "error|挑战已失效或已被使用,请重新获取。",
    en: "error|The challenge expired or was already used. Get a new one.",
  },
  "ssh.login_failed": {
    zh: "error|签名校验未通过:请确认签名的是当前挑战、命令带 `-n nvm-login`;agent 有多把密钥时显式选择已登记的那把。",
    en: "error|The signature did not verify. Make sure you signed the current challenge with `-n nvm-login`; with several keys in the agent, pick the registered one explicitly.",
  },
  "ssh.key_not_registered": {
    zh: "error|这把密钥未在本程序登记。先用密码登录,在「账户」页登记公钥。",
    en: "error|This key is not registered here. Sign in with the password first and register the public key on the Account page.",
  },
  "ssh.challenge_busy": {
    zh: "error|挑战发放过于频繁,请稍后再试。",
    en: "error|Challenges are being requested too quickly. Try again in a moment.",
  },
  "ssh.private_refused": {
    zh: "error|检测到私钥内容:本页面只提交公钥与签名,已清空该输入。私钥请留在你的设备上。",
    en: "error|Private key material detected: this page only submits public keys and signatures, and the input was cleared. Keep the private key on your device.",
  },
  "ssh.signature_bad_format": {
    zh: "error|这里只接受 `-----BEGIN SSH SIGNATURE-----` 格式的签名全文(或对应的 `.sig` 文件内容)。",
    en: "error|Only the full armored `-----BEGIN SSH SIGNATURE-----` text (or the matching `.sig` file) belongs here.",
  },
  "ssh.public_key_bad_format": {
    zh: "error|公钥应为一行,以 `ssh-ed25519`、`ecdsa-sha2-nistp256` 或 `ecdsa-sha2-nistp384` 开头。",
    en: "error|The public key is one line starting with `ssh-ed25519`, `ecdsa-sha2-nistp256` or `ecdsa-sha2-nistp384`.",
  },
  "ssh.keys.title": { zh: "SSH 登录密钥", en: "SSH sign-in keys" },
  "ssh.keys.intro": {
    zh: "登记公钥后,可用对应私钥签名登录;私钥永远保存在你的设备上。",
    en: "Register a public key to sign in with the matching private key; the private key always stays on your device.",
  },
  "ssh.keys.empty": { zh: "未登记任何 SSH 密钥。", en: "No SSH keys registered." },
  "ssh.keys.add_title": { zh: "登记公钥", en: "Register a public key" },
  "ssh.keys.public_key": {
    zh: "公钥(一行,以 `ssh-ed25519` 等开头)",
    en: "Public key (one line starting with `ssh-ed25519`, …)",
  },
  "ssh.keys.comment": { zh: "备注(可选)", en: "Comment (optional)" },
  "ssh.keys.add_submit": { zh: "登记", en: "Register" },
  "ssh.keys.delete": { zh: "删除", en: "Delete" },
  "ssh.keys.delete_last": {
    zh: "这是最后一把已登记的 SSH 公钥。删除后，登录页不再提供 SSH 登录。",
    en: "This is the last registered SSH public key. After deletion, the sign-in page no longer offers SSH sign-in.",
  },
  "ssh.keys.delete_confirm": {
    zh: "删除后，这把密钥不能再登录。其他浏览器会退出登录，这个浏览器保持登录，并留在账户页。确定删除?",
    en: "After deletion, this key can no longer sign in. Other browsers sign out. This browser stays signed in, and you remain on the Account page. Delete it?",
  },
  "ssh.keys.sign_command": {
    zh: "这把密钥的签名命令(把 {challenge_placeholder} 换成登录页当前的挑战字符串)",
    en: "The sign command of this key (replace {challenge_placeholder} with the current challenge on the sign-in page)",
  },
  "ssh.keys.added": {
    zh: "已登记 {fingerprint}。",
    en: "Registered {fingerprint}.",
  },
  "ssh.keys.deleted": { zh: "已删除。", en: "Deleted." },
  "ssh.keys.algorithm": { zh: "算法", en: "Algorithm" },
  "ssh.keys.fingerprint": { zh: "指纹", en: "Fingerprint" },
  "ssh.keys.added_at": { zh: "添加时间", en: "Added" },
  "ssh.key_invalid": {
    zh: "error|公钥无法识别:{reason}。",
    en: "error|The public key is not accepted: {reason}.",
  },
  "ssh.key_invalid.bad_format": { zh: "格式不正确", en: "malformed" },
  "ssh.key_invalid.unsupported_type": {
    zh: "不支持的类型(仅收 ed25519 与 ecdsa-p256/p384):{algorithm}",
    en: "unsupported type (only ed25519 and ecdsa-p256/p384 are accepted): {algorithm}",
  },
  "ssh.key_invalid.weak_key": {
    zh: "弱或无效的密钥点,不安全,已拒绝",
    en: "weak or invalid key point, refused as unsafe",
  },
  "ssh.key_exists": {
    zh: "error|这把公钥已登记({fingerprint})。",
    en: "error|This public key is already registered ({fingerprint}).",
  },
  "ssh.key_limit": {
    zh: "error|最多登记 5 把密钥。",
    en: "error|At most 5 keys can be registered.",
  },
  "ssh.key_not_found": {
    zh: "error|这把密钥不存在,请刷新列表。",
    en: "error|This key does not exist; refresh the list.",
  },
  "ssh.keys_unsaved": {
    zh: "error|SSH 密钥保存失败,文件未变更:{write_reason}。",
    en: "error|Saving the SSH keys failed and the file was not changed: {write_reason}.",
  },
  "ssh.keys.load_failed": {
    zh: "error|SSH 密钥列表读取失败。",
    en: "error|The SSH key list could not be read.",
  },

  "password.title": {
    zh: "请修改初始管理员密码",
    en: "Change the initial administrator password",
  },
  "password.title_voluntary": {
    zh: "账户",
    en: "Account",
  },
  "password.section": {
    zh: "修改密码",
    en: "Change password",
  },
  "password.intro": {
    zh: "当前使用的是 compose 中设置的初始密码。请先修改密码,之后才能开始更新。",
    en: "You signed in with the initial password from the compose file. Change the password before starting an update.",
  },
  "password.rule.too_short": {
    zh: "error|新密码至少需要 8 个字符。",
    en: "error|The new password must have at least 8 characters.",
  },
  "password.rule.too_long": {
    zh: "error|新密码不能超过 1024 字节。",
    en: "error|The new password must not exceed 1024 bytes.",
  },
  "password.rule.whitespace": {
    zh: "error|新密码不能全是空白。",
    en: "error|The new password must not consist only of whitespace.",
  },
  "password.rule.placeholder": {
    zh: "error|新密码不能以 `<` 开头并以 `>` 结尾。",
    en: "error|The new password must not both start with `<` and end with `>`.",
  },
  "password.rule.same_as_initial": {
    zh: "error|请换一个与当前密码不同的新密码。",
    en: "error|Choose a new password that is different from the current one.",
  },
  "password.mismatch": {
    zh: "error|两次输入的新密码不同。",
    en: "error|The two new passwords do not match.",
  },
  "password.changed": {
    zh: "密码已修改。其他已登录的会话已失效。",
    en: "The password was changed. Other signed-in sessions have been signed out.",
  },

  "setup.blocked": {
    zh: "error|无法开始更新:设置或启动检查有问题。请按页面顶部的提示修正后重启 App。",
    en: "error|Cannot start an update: there is a problem with the settings or the startup checks. Fix it as described at the top of the page, then restart the app.",
  },
  "update.disabled_setup": {
    zh: "info|设置或启动检查有问题,修正前无法更新。详见页面顶部的提示。",
    en: "info|Updates are unavailable until the problem with the settings or the startup checks is fixed. See the notice at the top of the page.",
  },
  "update.disabled_no_version": {
    zh: "请先在下拉框中选择版本。",
    en: "Select a version in the list first.",
  },
  "plan.source_invalid.version_format": {
    zh: "error|版本号须为 2 到 4 段用点分隔的数字,例如 `580.173.02`。",
    en: "error|The version must be two to four groups of digits separated by dots, for example `580.173.02`.",
  },
  "plan.source_invalid.version_below_470": {
    zh: "error|官网来源只接受 470 及以上的版本。请在「更新驱动」页的下拉框中选择版本。",
    en: "error|Official versions must be 470 or later. Select a version in the list on the Update driver page.",
  },
  "plan.source_invalid.branch_unbuildable": {
    zh: "error|无法开始更新:{branch} 开头的官方版本无法在当前主机上构建:{unbuildable_reason}。请刷新「更新驱动」页后,在下拉框中选择已列出的版本。",
    en: "error|Cannot start the update: official versions starting with {branch} cannot be built on this host: {unbuildable_reason}. Reload the Update driver page, then select a listed version in the list.",
  },
  "plan.source_invalid.branch_undecidable": {
    zh: "error|无法开始更新:无法判断所选版本能否在当前内核上构建。",
    en: "error|Cannot start the update: it cannot be determined whether the selected version builds on the current kernel.",
  },
  "plan.source_invalid.upload_incomplete": {
    zh: "error|上传的文件不完整,缺少:{packages}。",
    en: "error|The uploaded files are incomplete. Missing: {packages}.",
  },
  "plan.source_invalid.upload_problem": {
    zh: "error|上传的文件有问题,请查看上传窗口里的提示。",
    en: "error|There is a problem with the uploaded files. See the messages in the upload window.",
  },
  "plan.source_invalid.upload_version_required": {
    zh: "error|请选择或输入驱动版本。",
    en: "error|Select or enter the driver version.",
  },
  "plan.source_invalid.upload_version_not_candidate": {
    zh: "error|版本 {version} 不是从安装包读出的版本之一,请在上传窗口中重新选择。",
    en: "error|Version {version} is not one of the versions read from the installer. Select it again in the upload window.",
  },
  "plan.source_invalid.upload_group_gone": {
    zh: "error|上传的文件已不在暂存中,请重新上传。",
    en: "error|The uploaded files are no longer staged. Upload them again.",
  },
  "plan.source_invalid.local_row_missing": {
    zh: "error|所选本地驱动已不存在,请刷新本地驱动表。",
    en: "error|The selected local driver no longer exists. Reload Local drivers.",
  },
  "plan.used": {
    zh: "error|无法开始更新:此确认已提交过或已失效。请重新开始更新。",
    en: "error|Cannot start the update: this confirmation was already submitted or is no longer valid. Start the update again.",
  },
  "plan.button_invalid": {
    zh: "error|无法开始更新:确认选项与当前原版驱动的保存状态不符。请重新开始更新。",
    en: "error|Cannot start the update: the chosen option does not match whether an original driver is saved. Start the update again.",
  },
  "submit.failed": {
    zh: "error|无法开始更新:{submit_reason}。驱动文件未改动。",
    en: "error|Cannot start the update: {submit_reason}. The driver file was not changed.",
  },
  "plan.changed": {
    zh: "error|title|无法开始更新:计划已变化\nbody|与弹窗中确认的内容相比,以下项已变化:{changed_items}。驱动文件未改动。\naction|请重新开始更新,并在弹窗中确认新的计划。",
    en: "error|title|Cannot start the update: the plan has changed\nbody|These items differ from what was confirmed in the dialog: {changed_items}. The driver file was not changed.\naction|Start the update again and confirm the new plan in the dialog.",
  },
  "plan.changed.one": {
    zh: "error|title|无法开始更新:计划已变化\nbody|与弹窗中确认的内容相比,以下项已变化:{changed_items}。驱动文件未改动。\naction|请重新开始更新,并在弹窗中确认新的计划。",
    en: "error|title|Cannot start the update: the plan has changed\nbody|This item differs from what was confirmed in the dialog: {changed_items}. The driver file was not changed.\naction|Start the update again and confirm the new plan in the dialog.",
  },
  "plan.changed.after_open": {
    zh: "error|title|无法开始更新:计划已变化\nbody|页面打开之后,以下项已变化:{changed_items}。驱动文件未改动。\naction|请重新开始更新,并在弹窗中确认新的计划。",
    en: "error|title|Cannot start the update: the plan has changed\nbody|This item changed after the page was opened: {changed_items}. The driver file was not changed.\naction|Start the update again and confirm the new plan in the dialog.",
  },
  "plan.changed.item.source": { zh: "来源", en: "Source" },
  "plan.changed.item.version": { zh: "版本", en: "Version" },
  "plan.changed.item.kernel": { zh: "当前内核", en: "Current kernel" },
  "plan.changed.item.truenas_version": { zh: "TrueNAS 版本", en: "TrueNAS version" },
  "plan.changed.item.original": { zh: "是否保存原版", en: "Save original driver" },
  "plan.changed.item.gpus": { zh: "所选显卡", en: "Selected GPUs" },
  "submit.runner_copy_failed": {
    zh: "无法把任务程序复制到数据目录",
    en: "the task program could not be copied to the data directory",
  },
  "submit.state_unwritable": {
    zh: "无法写入任务状态文件",
    en: "the task state file could not be written",
  },
  "submit.task_dir_failed": {
    zh: "无法在数据目录中建立任务目录",
    en: "the task directory could not be created in the data directory",
  },
  "submit.unit_not_created": {
    zh: "`systemd-run` 以退出码 {code} 结束,主机上没有建立任务单元,可能是数据目录所在的数据集禁止执行程序",
    en: "`systemd-run` ended with exit code {code}, and no task unit was created on the host; the dataset holding the data directory may not allow executing programs",
  },
  "submit.exec_failed": {
    zh: "主机无法执行数据目录里的任务程序",
    en: "the host could not run the task program in the data directory",
  },

  "upload.busy.text": {
    zh: "error|已有文件正在上传。请在当前上传完成后重试。进行中的上传不受影响。",
    en: "error|A file is already being uploaded. Try again after the current upload finishes. The upload in progress is not affected.",
  },
  "upload.group_replaced": {
    zh: "error|这组上传已不再接收文件:它已被新的一组取代,或已用于开始更新。请重新上传全部文件。",
    en: "error|This upload no longer accepts files: it was replaced by a newer one or used to start an update. Upload all files again.",
  },
  "upload.interrupted": {
    zh: "error|{file_name} 的上传中断,已写入的部分已删除。请重新上传这个文件。",
    en: "error|The upload of {file_name} was interrupted, and the part already written has been deleted. Upload this file again.",
  },
  "upload.receiving": {
    zh: "正在上传 {file_name}:{done} MB / {total} MB。完成后才能继续。",
    en: "Uploading {file_name}: {done} MB of {total} MB. You can continue when it finishes.",
  },
  "upload.file_ok": { zh: "{file_name} 已接收。", en: "{file_name} was received." },
  "upload.group.missing": {
    zh: "warning|还缺少:{packages}。",
    en: "warning|Still missing: {packages}.",
  },
  "upload.group.run_missing": {
    zh: "warning|还缺少驱动安装包 `.run`。",
    en: "warning|The driver installer `.run` is still missing.",
  },
  "upload.group.versions_differ": {
    zh: "warning|4 个 toolkit 包的版本不同:{versions}。请上传同一版本的 4 个包。",
    en: "warning|The four toolkit packages have different versions: {versions}. Upload four packages of the same version.",
  },
  "upload.group.toolkit_too_old": {
    zh: "warning|驱动 {driver_version} 为 615 及以上版本,toolkit 须不低于 1.20.1,当前为 {toolkit_version}。请上传 1.20.1 或更新的 toolkit。",
    en: "warning|Driver {driver_version} is 615 or later, so the toolkit must be 1.20.1 or later; the uploaded toolkit is {toolkit_version}. Upload toolkit 1.20.1 or later.",
  },
  "upload.version.detected": {
    zh: "读出的驱动版本:{version}。",
    en: "Driver version read from the file: {version}.",
  },
  "upload.version.choose": {
    zh: "warning|安装包的标签里有多个版本号,请选择本驱动的版本。",
    en: "warning|The installer label contains several version numbers. Select the version of this driver.",
  },
  "upload.version.enter": {
    zh: "warning|无法从安装包读出版本号,请输入本驱动的版本(例如 `580.173.02`)。",
    en: "warning|The version could not be read from the installer. Enter the version of this driver (for example `580.173.02`).",
  },
  "upload.version.invalid": {
    zh: "error|版本号须为 2 到 4 段用点分隔的数字。",
    en: "error|The version must be two to four groups of digits separated by dots.",
  },

  "delete.refused.in_use": {
    zh: "error|{version}({source})正在使用,不能删除。如需删除,请先更新到其他版本。",
    en: "error|{version} ({source}) is in use and cannot be deleted. To delete it, update to another version first.",
  },
  // A missing version or kind uses the row id for the whole version/source label.
  "delete.refused.in_use.id_only": {
    zh: "error|{version}正在使用,不能删除。如需删除,请先更新到其他版本。",
    en: "error|{version} is in use and cannot be deleted. To delete it, update to another version first.",
  },
  "delete.refused.kept": {
    zh: "error|{version}(原版)已设为保留,不能删除。如需删除,请先关闭该行的「保留」。",
    en: "error|{version} (original) is kept and cannot be deleted. To delete it, turn off Keep for that row first.",
  },
  "delete.refused.unknown": {
    zh: "error|{version}({source})的校验值或目标内核无法读取,不能删除。",
    en: "error|The checksum or target kernel of {version} ({source}) cannot be read, so it cannot be deleted.",
  },
  "delete.refused.unknown.id_only": {
    zh: "error|{version}的校验值或目标内核无法读取,不能删除。",
    en: "error|The checksum or target kernel of {version} cannot be read, so it cannot be deleted.",
  },
  "delete.row_missing": {
    zh: "error|所选的本地驱动已不存在,请刷新本地驱动表。",
    en: "error|The selected local driver no longer exists. Reload Local drivers.",
  },
  "delete.task_running": {
    zh: "error|有未结束的任务,结束后才能删除本地驱动。",
    en: "error|A task is still running. Local drivers can be deleted after it finishes.",
  },
  "delete.task_state_unknown": {
    zh: "error|无法确认是否有未结束的任务,暂时不能删除本地驱动。",
    en: "error|It cannot be confirmed whether a task is still running, so local drivers cannot be deleted for now.",
  },
  "delete.canonical_unreadable": {
    zh: "error|无法读取正式路径上驱动文件的校验值,暂时不能删除本地驱动。",
    en: "error|The checksum of the driver file at the permanent path cannot be read, so local drivers cannot be deleted for now.",
  },
  "delete.done": {
    zh: "已删除 {row_count} 个本地驱动。",
    en: "Deleted {row_count} local drivers.",
  },
  "delete.done.one": { zh: "已删除 1 个本地驱动。", en: "Deleted the local driver." },
  "delete.failed": {
    zh: "error|删除本地驱动时无法写入数据目录:{reason}。出错前已删除 {deleted_count} 个本地驱动。其余所选的本地驱动仍在表中。",
    en: "error|Deleting local drivers failed because the data directory could not be written: {reason}. Local drivers deleted before the error: {deleted_count}. The other selected local drivers are still in the table.",
  },
  "keep.not_original": { zh: "error|只有原版驱动可以设为保留。", en: "error|Only original drivers can be kept." },
  "keep.row_unreadable": {
    zh: "error|这一行的记录无法读取,不能更改「保留」。",
    en: "error|The record of this row cannot be read, so Keep cannot be changed.",
  },
  "keep.unsaved": {
    zh: "error|无法保存「保留」的更改,因为数据目录无法写入:{reason}。这一行的记录未改动。",
    en: "error|The change to Keep could not be saved because the data directory could not be written: {reason}. The record of this row was not changed.",
  },
  "drivers.row_unknown": {
    zh: "此行的校验值或目标内核无法读取。",
    en: "The checksum or target kernel of this row cannot be read.",
  },
  "drivers.delete_disabled_kept": {
    zh: "已设为保留。关闭「保留」后才能删除。",
    en: "Kept. Turn off Keep to delete it.",
  },
  "drivers.delete_disabled_in_use": {
    zh: "正在使用的驱动不能删除。如需删除,请先更新到其他版本。",
    en: "The driver in use cannot be deleted. To delete it, update to another version first.",
  },

  "auth.required": {
    zh: "error|登录已失效,请重新登录。",
    en: "error|Your session has ended. Sign in again.",
  },
  "auth.origin_mismatch": {
    zh: "error|请求来源与本程序的地址不一致,已被拒绝。请直接打开本程序的地址后重试。",
    en: "error|The request origin does not match this program's address and was refused. Open this program's address directly and try again.",
  },
  "auth.password_change_required": {
    zh: "error|请先修改初始管理员密码。",
    en: "error|Change the initial administrator password first.",
  },
  "request.invalid": { zh: "error|请求内容不合法。", en: "error|The request is not valid." },
  "request.too_large": { zh: "error|请求内容过大。", en: "error|The request is too large." },
  "request.not_json": {
    zh: "error|请求格式不正确。",
    en: "error|The request format is not correct.",
  },
  "not_found": {
    zh: "error|找不到请求的对象。",
    en: "error|The requested item was not found.",
  },
  "internal": {
    zh: "error|本程序内部出错:{reason}。",
    en: "error|An internal error occurred in this program: {reason}.",
  },
  "internal.request_failed": { zh: "请求处理失败", en: "request handling failed" },
  "internal.data_dir_unreadable": {
    zh: "数据目录无法读取",
    en: "the data directory could not be read",
  },

  "app.title": {
    zh: "TrueNAS NVIDIA Driver Manager",
    en: "TrueNAS NVIDIA Driver Manager",
  },
  "app.purpose": {
    zh: "在 TrueNAS 上更新 NVIDIA 驱动。",
    en: "Update the NVIDIA driver on TrueNAS.",
  },
  "nav.home": { zh: "主页", en: "Home" },
  "nav.update": { zh: "更新驱动", en: "Update driver" },
  "nav.drivers": { zh: "本地驱动", en: "Local drivers" },
  "nav.tasks": { zh: "任务", en: "Tasks" },
  "nav.change_password": { zh: "账户", en: "Account" },
  "nav.logout": { zh: "退出登录", en: "Sign out" },

  "nav.tasks.active": { zh: "有正在进行的任务", en: "Task in progress" },
  "nav.tasks.interrupted": { zh: "有中断的任务待处理", en: "Interrupted task needs attention" },

  "theme.control": { zh: "主题", en: "Theme" },
  "theme.option.plate-light": { zh: "铭牌 · 亮色", en: "Plate · Light" },
  "theme.option.plate-dark": { zh: "铭牌 · 暗色", en: "Plate · Dark" },
  "theme.option.focused-light": { zh: "聚焦 · 亮色", en: "Focused · Light" },
  "theme.option.focused-dark": { zh: "聚焦 · 暗色", en: "Focused · Dark" },
  "page.login": { zh: "登录", en: "Sign in" },
  "page.task": { zh: "任务 {task_id}", en: "Task {task_id}" },
  "dialog.precheck": { zh: "更新前检查", en: "Pre-update check" },
  "dialog.confirm": { zh: "确认更新", en: "Confirm the update" },
  "dialog.upload": { zh: "上传驱动", en: "Upload driver" },
  "lang.control": { zh: "语言", en: "Language" },
  "lang.option.zh": { zh: "中文", en: "中文" },
  "lang.option.en": { zh: "English", en: "English" },

  "login.username": { zh: "用户名", en: "Username" },
  "login.password": { zh: "密码", en: "Password" },
  "login.submit": { zh: "登录", en: "Sign in" },

  "login.intro": {
    zh: "使用部署本应用时设置的管理员账号与密码登录。",
    en: "Sign in with the administrator account and password configured when this app was deployed.",
  },
  "login.help.summary": {
    zh: "没有账号?初始密码在哪里?",
    en: "No account? Where is the initial password?",
  },

  "login.help.hint": {
    zh: "没有账号?请联系部署本应用的人获取管理员账号;详细说明可在下方展开。",
    en: "No account? Contact whoever deployed this app for an administrator account; expand the help below for details.",
  },
  "login.help.no_account": {
    zh: "没有账号:请联系**部署本应用的人**,获取本应用的管理员账号。",
    en: "No account: contact **whoever deployed this app** for this app's administrator account.",
  },
  "login.help.self_hosted": {
    zh: "自己部署的:在 TrueNAS 中打开本应用的编辑配置,查看 compose/YAML 里的环境变量。用户名是 `NVM_INITIAL_ADMIN_USERNAME`(默认 `admin`,可以更改),初始密码是 `NVM_INITIAL_ADMIN_PASSWORD` 的值;首次登录后会要求修改密码。",
    en: "Self-deployed: open this app's edit configuration in TrueNAS and look at the environment variables in the compose/YAML. The username is `NVM_INITIAL_ADMIN_USERNAME` (default `admin`, changeable), and the initial password is the value of `NVM_INITIAL_ADMIN_PASSWORD`; the first sign-in asks you to change the password.",
  },
  "login.help.after_reinstall": {
    zh: "重装或升级时保留了数据目录的,账号和修改过的密码继续有效;初始配置不会覆盖已有凭据。",
    en: "If a reinstall or upgrade kept the data directory, the account and the changed password keep working; the initial configuration never overwrites existing credentials.",
  },
  "login.help.forgot_password": {
    zh: "忘记密码:请联系有 TrueNAS Shell 和本应用数据目录权限的管理员,按现有运维说明重置管理员账号。按该说明删除凭证文件并重建账号时,已登记的 SSH 公钥会被清除,之后需要重新登记。",
    en: "Forgot the password: contact an administrator who has TrueNAS Shell and this app's data directory access, and follow the existing operations instructions to reset the administrator account. Rebuilding the account that way clears the registered SSH public keys, which then have to be registered again.",
  },
  "password.new": { zh: "新密码", en: "New password" },
  "password.repeat": { zh: "再次输入新密码", en: "Repeat the new password" },
  "password.submit": { zh: "修改密码", en: "Change password" },

  "home.loaded_version": {
    zh: "内核里加载的驱动版本",
    en: "Driver version loaded in the kernel",
  },
  "home.loaded_version.none": {
    zh: "未加载 NVIDIA 驱动模块",
    en: "No NVIDIA driver module is loaded",
  },
  "home.canonical_version": {
    zh: "正式路径上文件的版本",
    en: "Version of the file at the permanent path",
  },
  "home.canonical_checksum": {
    zh: "正式路径上文件的校验值",
    en: "Checksum of the file at the permanent path",
  },
  "home.truenas_version": { zh: "TrueNAS 版本", en: "TrueNAS version" },
  "home.kernel": { zh: "当前内核", en: "Current kernel" },
  "home.link": { zh: "驱动扩展链接", en: "Driver extension link" },
  "home.link.ok": { zh: "存在,指向正式路径", en: "Present; points to the permanent path" },
  "home.link.missing": { zh: "不存在", en: "Not present" },
  "home.link.elsewhere": { zh: "指向 {link_target}", en: "Points to {link_target}" },
  "home.original": {
    zh: "当前内核与 TrueNAS 版本的原版驱动",
    en: "Original driver for the current kernel and TrueNAS version",
  },
  "home.original.saved": {
    zh: "已保存:{original}(保存于 {date})",
    en: "Saved: {original} (saved {date})",
  },
  "home.original.none": { zh: "未保存", en: "Not saved" },
  "home.active_task": {
    zh: "有未结束的任务:{task_id}。",
    en: "A task is still running: {task_id}.",
  },
  "home.active_task.unknown": {
    zh: "有未结束的任务:{task_id},状态 UNKNOWN。",
    en: "A task is still running: {task_id}, status UNKNOWN.",
  },
  "home.open_task": { zh: "查看任务", en: "View the task" },

  "update.catalog_label": { zh: "NVIDIA 官网版本", en: "Official NVIDIA versions" },
  "update.button": { zh: "检查并继续", en: "Check and continue" },
  "update.link_text": { zh: "NVIDIA 官网链接", en: "NVIDIA website link" },
  "update.upload_button": { zh: "上传 .run", en: "Upload .run" },
  "upload.files_label": {
    zh: "驱动安装包与 toolkit 包",
    en: "Driver installer and toolkit packages",
  },
  "upload.start": { zh: "上传", en: "Upload" },
  "upload.restart": { zh: "重新开始上传", en: "Start the upload over" },
  "upload.version_label": { zh: "驱动版本", en: "Driver version" },
  "upload.update": { zh: "检查并继续", en: "Check and continue" },
  "upload.close": { zh: "关闭", en: "Close" },
  "confirm.source": { zh: "来源", en: "Source" },
  "confirm.version": { zh: "版本", en: "Version" },
  "confirm.kernel": { zh: "当前内核", en: "Current kernel" },
  "confirm.truenas": { zh: "TrueNAS 版本", en: "TrueNAS version" },
  "confirm.original": { zh: "是否保存原版", en: "Save original driver" },
  "confirm.source.official": { zh: "NVIDIA 官网", en: "NVIDIA website" },
  "confirm.source.upload": {
    zh: "上传 · 指纹 {fingerprint}(校验值 {checksum})",
    en: "Upload · fingerprint {fingerprint} (checksum {checksum})",
  },
  "confirm.source.local": {
    zh: "本地驱动 · {source}(校验值 {checksum})",
    en: "Local driver · {source} (checksum {checksum})",
  },
  "confirm.upload_trust": {
    zh: "你上传的驱动安装包会在这台主机上构建并加载进内核。请只使用你信任的安装包。",
    en: "The driver installer you uploaded will be built on this host and loaded into its kernel. Only use an installer you trust.",
  },
  "confirm.community_patch": {
    zh: "此驱动使用来自 Debian 的社区兼容补丁。它不支持用户 I/O 内存映射；显示热插拔行为可能不同。在启用 IBT 的系统上无法加载。Kepler 上的加载、显示及 CUDA 工作负载尚未验证。",
    en: "This driver uses Debian community compatibility patches. User I/O memory mappings are unavailable, and display hotplug behavior may differ. The driver cannot load on systems with IBT enabled. Loading, display, and CUDA workloads on Kepler have not been verified.",
  },
  "confirm.original.ask": {
    zh: "没有可用的已保存原版,请选择",
    en: "No usable saved original driver; choose below",
  },
  "confirm.original.unsavable": {
    zh: "正式路径上的驱动文件不能保存为原版驱动。本程序无法从这个文件确定驱动版本和目标内核。",
    en: "The driver file at the permanent path cannot be saved as the original driver. This program cannot tell which driver version and target kernel this file is for.",
  },
  "confirm.save": { zh: "保存原版并开始", en: "Save original and start" },
  "confirm.skip": { zh: "不保存，直接开始", en: "Start without saving" },
  "confirm.how.official": {
    zh: "这个来源会从 NVIDIA 官网下载安装包,再在本机构建。",
    en: "This source downloads the installer from the NVIDIA website, then builds it on this host.",
  },
  "confirm.how.upload": {
    zh: "这个来源使用已上传的安装包,不会再下载。",
    en: "This source uses the uploaded installer and does not download it again.",
  },
  "confirm.how.local": {
    zh: "这个来源使用已有的本地驱动,既不下载,也不再构建。",
    en: "This source uses a local driver that is already here. It neither downloads nor builds.",
  },
  "confirm.start": { zh: "开始更新", en: "Start update" },
  "confirm.back": { zh: "返回选择", en: "Back to selection" },
  "confirm.abort": { zh: "返回选择", en: "Return to the choices" },
  "precheck.close": { zh: "关闭", en: "Close" },
  "confirm.close": { zh: "关闭", en: "Close" },
  "update.gpu_select": {
    zh: "选择要用这个驱动的显卡。",
    en: "Select the GPUs that will use this driver.",
  },
  "update.gpu_option": { zh: "{gpu_name}({pci_address})", en: "{gpu_name} ({pci_address})" },
  "catalog.gpu.not_listed": {
    zh: "info|{gpu_name} 最后支持的是 {branch} 驱动,更新的版本不列出。",
    en: "info|{gpu_name} is last supported by the {branch} driver; newer versions are not listed.",
  },
  "catalog.gpu.not_listed.from": {
    zh: "info|{gpu_name} 从 {branch} 驱动起才支持,更早的版本不列出。",
    en: "info|{gpu_name} is supported starting with the {branch} driver; earlier versions are not listed.",
  },
  "catalog.no_usable_version": {
    zh: "error|没有能驱动所选显卡的版本。",
    en: "error|No version can drive the selected GPUs.",
  },
  "catalog.no_usable_version.one": {
    zh: "error|没有能驱动所选显卡的版本。",
    en: "error|No version can drive the selected GPU.",
  },

  "drivers.col.version": { zh: "版本", en: "Version" },
  "drivers.col.source": { zh: "来源", en: "Source" },
  "drivers.col.date": { zh: "构建日期", en: "Build date" },
  "drivers.col.size": { zh: "大小", en: "Size" },
  "drivers.col.kernel": { zh: "目标内核", en: "Target kernel" },
  "drivers.col.status": { zh: "状态", en: "Status" },
  "drivers.col.keep": { zh: "保留", en: "Keep" },
  "drivers.source.official": { zh: "官网构建", en: "Official build" },
  "drivers.source.upload": {
    zh: "上传 · 指纹 {fingerprint}",
    en: "Upload · fingerprint {fingerprint}",
  },
  "drivers.source.original": { zh: "原版", en: "Original" },
  "drivers.status.in_use": { zh: "在用", en: "In use" },
  "drivers.status.available": { zh: "可用", en: "Available" },
  "drivers.status.kernel_mismatch": { zh: "内核不匹配", en: "Kernel mismatch" },
  "drivers.keep.on": { zh: "保留:开", en: "Keep: on" },
  "drivers.keep.off": { zh: "保留:关", en: "Keep: off" },
  "drivers.update_row": { zh: "用这个版本检查并继续", en: "Check this version and continue" },
  "drivers.delete_row": { zh: "删除", en: "Delete" },
  "drivers.delete_selected": { zh: "删除所选", en: "Delete selected" },
  "drivers.select_row": { zh: "选择 {version}({source})", en: "Select {version} ({source})" },
  "drivers.select_all": { zh: "全选", en: "Select all" },
  "drivers.sort": { zh: "按{column}排序", en: "Sort by {column}" },
  "drivers.size_value": { zh: "{size} MB", en: "{size} MB" },
  "drivers.empty": {
    zh: "还没有本地驱动。构建成功的驱动与保存的原版驱动会出现在这里。",
    en: "There are no local drivers yet. Built drivers and saved original drivers appear here.",
  },
  "delete.confirm_button": { zh: "删除", en: "Delete" },
  "delete.cancel_button": { zh: "取消", en: "Cancel" },

  "tasks.col.id": { zh: "任务号", en: "Task" },
  "tasks.col.status": { zh: "状态", en: "Status" },
  "tasks.col.phase": { zh: "阶段", en: "Stage" },
  "tasks.col.source": { zh: "来源", en: "Source" },
  "tasks.col.version": { zh: "版本", en: "Version" },
  "tasks.col.started": { zh: "开始时间", en: "Started" },
  "tasks.col.finished": { zh: "结束时间", en: "Finished" },
  "tasks.col.discovered": { zh: "发现中断", en: "Interruption discovered" },
  "tasks.time.basis": {
    zh: "开始和结束时间来自任务记录。中断任务没有结束时间，发现中断的时刻单独列出。",
    en: "Start and end times come from the task record. An interrupted task has no end time; the moment the interruption was discovered is listed separately.",
  },
  "tasks.steps": { zh: "计划步骤", en: "Planned steps" },
  "tasks.time.not_interrupted": { zh: "未中断", en: "Not interrupted" },
  "task.latest_unknown": {
    zh: "warning|按记录的开始时间，无法确定最近的一次任务。",
    en: "warning|The recorded start times do not identify one latest task.",
  },
  "tasks.source.official": { zh: "NVIDIA 官网", en: "NVIDIA website" },
  "tasks.source.upload": {
    zh: "上传 · 指纹 {fingerprint}",
    en: "Upload · fingerprint {fingerprint}",
  },
  "tasks.source.local": { zh: "本地驱动 · {source}", en: "Local driver · {source}" },
  "task.status": { zh: "状态", en: "Status" },
  "task.phase": { zh: "当前阶段", en: "Current stage" },
  "task.download": { zh: "下载进度", en: "Download progress" },
  "task.log": { zh: "日志尾部", en: "Log tail" },
  "task.error_code": { zh: "错误码与说明", en: "Error code and explanation" },
  "task.raw_output": { zh: "外部命令原文", en: "External command output" },
  "task.details": { zh: "任务详情", en: "Task details" },
  "task.checks": {
    zh: "成功检查的观测值",
    en: "Post-update check readings",
  },
  "task.state_file_kept": {
    zh: "无法读取的原状态文件已另存为 {file_name}。",
    en: "The unreadable original state file was kept as {file_name}.",
  },
  "task.none": { zh: "还没有任务。", en: "There are no tasks yet." },

  "notice.version_changed": {
    zh: "warning|title|驱动版本已变更\nbody|已加载的 NVIDIA 驱动由 {old} 变为 {new}。以下应用申请了显卡,其中在变更前启动的应用可能仍在使用变更前的驱动库,使用显卡时可能出错(例如 “Failed to initialize NVML: Driver/library version mismatch”)。本程序不会自动重启这些应用。\naction|请在方便时重启以下应用。上方通知如有操作,请先完成该操作。\nlist|{name} · {type}",
    en: "warning|title|Driver version changed\nbody|The loaded NVIDIA driver changed from {old} to {new}. The applications below requested the GPU. Those started before the change may still use the driver libraries from before the change, and GPU features in them may fail (for example “Failed to initialize NVML: Driver/library version mismatch”). This program does not restart them automatically.\naction|Restart the applications below when convenient. If the notice above lists an action, complete it first.\nlist|{name} · {type}",
  },

  "notice.gpu_in_use": {
    zh: "error|title|无法更新:显卡正被占用\nbody|显卡正被占用,驱动文件未改动。\naction|以下是申请了显卡的应用。请停止其中正在使用显卡的应用,然后重新开始更新。\nlist|{name} · {type}\nnote|如已停止正在使用显卡的应用,仍提示显卡正被占用,占用者可能是主机上直接运行的进程,或显卡正用于显示输出。",
    en: "error|title|Cannot update: GPU in use\nbody|The GPU is in use. The driver file was not changed.\naction|The applications below requested the GPU. Stop those that are using the GPU, then start the update again.\nlist|{name} · {type}\nnote|If the GPU is still reported in use after the applications using it are stopped, it may be held by a process running directly on the host, or by a display output.",
  },
  "notice.gpu_in_use.empty_note": {
    zh: "note|未找到申请了显卡的应用。占用者可能是主机上直接运行的进程,或显卡正用于显示输出。",
    en: "note|No application that requested the GPU was found. The GPU may be held by a process running directly on the host, or by a display output.",
  },
  "notice.gpu_in_use.unreadable": {
    zh: "error|title|无法更新:无法读取显卡占用状态\nbody|无法读取显卡占用状态:{read_reason}。驱动文件未改动。\naction|以下是申请了显卡的应用。请停止其中正在使用显卡的应用,然后重新开始更新。\nlist|{name} · {type}",
    en: "error|title|Cannot update: GPU usage state could not be read\nbody|The GPU usage state could not be read: {read_reason}. The driver file was not changed.\naction|The applications below requested the GPU. Stop those that are using the GPU, then start the update again.\nlist|{name} · {type}",
  },
  "notice.gpu_in_use.task": {
    zh: "error|title|更新已停止:显卡正被占用\nbody|显卡正被占用,驱动文件未改动。\naction|以下是申请了显卡的应用。请停止其中正在使用显卡的应用,然后重新开始更新。\nlist|{name} · {type}\nnote|如已停止正在使用显卡的应用,仍提示显卡正被占用,占用者可能是主机上直接运行的进程,或显卡正用于显示输出。",
    en: "error|title|Update stopped: GPU in use\nbody|The GPU is in use. The driver file was not changed.\naction|The applications below requested the GPU. Stop those that are using the GPU, then start the update again.\nlist|{name} · {type}\nnote|If the GPU is still reported in use after the applications using it are stopped, it may be held by a process running directly on the host, or by a display output.",
  },
  "notice.gpu_in_use.task_unreadable": {
    zh: "error|title|更新已停止:无法读取显卡占用状态\nbody|无法读取显卡占用状态:{read_reason}。驱动文件未改动。\naction|以下是申请了显卡的应用。请停止其中正在使用显卡的应用,然后重新开始更新。\nlist|{name} · {type}",
    en: "error|title|Update stopped: GPU usage state could not be read\nbody|The GPU usage state could not be read: {read_reason}. The driver file was not changed.\naction|The applications below requested the GPU. Stop those that are using the GPU, then start the update again.\nlist|{name} · {type}",
  },

  "notice.unload_refused": {
    zh: "error|title|更新已停止:驱动无法卸载\nbody|卸载 NVIDIA 驱动模块 {module} 被系统拒绝:{unload_reason}。已重新加载已卸载的驱动模块,驱动文件未改动,当前驱动仍为 {old}。\naction|以下是申请了显卡的应用。请停止其中正在使用显卡的应用,然后重新开始更新。\nlist|{name} · {type}\nnote|如已停止正在使用显卡的应用后仍无法卸载驱动模块,占用者可能是主机上直接运行的进程,或显卡正用于显示输出。",
    en: "error|title|Update stopped: driver could not be unloaded\nbody|Unloading the NVIDIA driver module {module} was refused: {unload_reason}. The driver modules already unloaded have been reloaded. The driver file was not changed; the driver is still {old}.\naction|The applications below requested the GPU. Stop those that are using the GPU, then start the update again.\nlist|{name} · {type}\nnote|If the driver modules still cannot be unloaded after the applications using the GPU are stopped, the GPU may be held by a process running directly on the host, or by a display output.",
  },
  "notice.unload_refused.reload_failed": {
    zh: "error|title|更新已停止:驱动无法卸载\nbody|卸载 NVIDIA 驱动模块 {module} 被系统拒绝:{unload_reason}。重新加载驱动模块失败:{reload_reason}。驱动文件未改动。已卸载的驱动模块没有全部重新加载,申请了显卡的应用可能无法启动。\naction|以下是申请了显卡的应用。请停止其中正在使用显卡的应用,然后重新开始更新。\nlist|{name} · {type}\nnote|如已停止正在使用显卡的应用后仍无法卸载驱动模块,占用者可能是主机上直接运行的进程,或显卡正用于显示输出。",
    en: "error|title|Update stopped: driver could not be unloaded\nbody|Unloading the NVIDIA driver module {module} was refused: {unload_reason}. Reloading the driver modules failed: {reload_reason}. The driver file was not changed. Not all of the unloaded driver modules were reloaded; applications that requested the GPU may fail to start.\naction|The applications below requested the GPU. Stop those that are using the GPU, then start the update again.\nlist|{name} · {type}\nnote|If the driver modules still cannot be unloaded after the applications using the GPU are stopped, the GPU may be held by a process running directly on the host, or by a display output.",
  },
  "notice.unload_refused.not_unloaded": {
    zh: "error|title|更新已停止:驱动无法卸载\nbody|卸载 NVIDIA 驱动模块 {module} 被系统拒绝:{unload_reason}。驱动文件未改动,当前驱动仍为 {old}。\naction|以下是申请了显卡的应用。请停止其中正在使用显卡的应用,然后重新开始更新。\nlist|{name} · {type}\nnote|如已停止正在使用显卡的应用后仍无法卸载驱动模块,占用者可能是主机上直接运行的进程,或显卡正用于显示输出。",
    en: "error|title|Update stopped: driver could not be unloaded\nbody|Unloading the NVIDIA driver module {module} was refused: {unload_reason}. The driver file was not changed; the driver is still {old}.\naction|The applications below requested the GPU. Stop those that are using the GPU, then start the update again.\nlist|{name} · {type}\nnote|If the driver modules still cannot be unloaded after the applications using the GPU are stopped, the GPU may be held by a process running directly on the host, or by a display output.",
  },

  "notice.update_complete": {
    zh: "info|title|更新完成\nbody|NVIDIA 驱动已更新为 {version},成功检查通过。",
    en: "info|title|Update complete\nbody|The NVIDIA driver is now {version}. The post-update check passed.",
  },

  "notice.update_complete.reload_warning": {
    zh: "warning|title|更新完成,但部分驱动模块没有重新加载\nbody|NVIDIA 驱动已更新为 {version},成功检查通过;但以下模块没有重新加载:{modules}。使用显卡的应用可能无法启动。\naction|请重启主机,或重新选择同一版本再更新一次。",
    en: "warning|title|Update complete, but some driver modules were not reloaded\nbody|The NVIDIA driver is now {version} and the post-update check passed, but these modules were not reloaded: {modules}. Applications that use the GPU may fail to start.\naction|Restart the host, or choose the same version again and update once more.",
  },

  "notice.restored": {
    zh: "warning|title|更新失败,已恢复原版驱动\nbody|驱动 {target} 未通过成功检查:{check_reason}。已自动恢复保存的原版驱动 {original}(保存于 {date})。",
    en: "warning|title|Update failed; original driver restored\nbody|Driver {target} failed the post-update check: {check_reason}. The saved original driver {original} (saved {date}) was restored automatically.",
  },

  "notice.restore_stopped": {
    zh: "error|title|更新失败,原版驱动恢复已中止\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 时显卡正被占用,恢复在卸载驱动模块之前中止,未卸载任何驱动模块。当前加载的驱动为 {current}。\naction|请停止以下应用中正在使用显卡的应用,然后在「本地驱动」中选择原版驱动 {original} 进行更新。\nlist|{name} · {type}\nnote|如已停止正在使用显卡的应用,仍提示显卡正被占用,占用者可能是主机上直接运行的进程,或显卡正用于显示输出。",
    en: "error|title|Update failed; restoring the original driver was stopped\nbody|Driver {target} failed the post-update check: {check_reason}. The GPU was in use when restoring the original driver {original}, so the restore stopped before any driver module was unloaded. The loaded driver is {current}.\naction|Stop the applications below that are using the GPU, then select the original driver {original} in Local drivers and update.\nlist|{name} · {type}\nnote|If the GPU is still reported in use after the applications using it are stopped, it may be held by a process running directly on the host, or by a display output.",
  },
  "notice.restore_stopped.unreadable": {
    zh: "error|title|更新失败,原版驱动恢复已中止\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 时无法读取显卡占用状态:{read_reason},恢复在卸载驱动模块之前中止,未卸载任何驱动模块。\naction|请停止以下应用中正在使用显卡的应用,然后在「本地驱动」中选择原版驱动 {original} 进行更新。\nlist|{name} · {type}",
    en: "error|title|Update failed; restoring the original driver was stopped\nbody|Driver {target} failed the post-update check: {check_reason}. The GPU usage state could not be read when restoring the original driver {original}: {read_reason}, so the restore stopped before any driver module was unloaded.\naction|Stop the applications below that are using the GPU, then select the original driver {original} in Local drivers and update.\nlist|{name} · {type}",
  },
  "notice.restore_stopped.unload_refused": {
    zh: "error|title|更新失败,原版驱动恢复已中止\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 时卸载驱动模块 {module} 被系统拒绝:{unload_reason},恢复在写入原版驱动文件之前中止。已重新加载已卸载的驱动模块,当前加载的驱动为 {current}。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver was stopped\nbody|Driver {target} failed the post-update check: {check_reason}. Unloading the driver module {module} was refused when restoring the original driver {original}: {unload_reason}, so the restore stopped before the original driver file was written. The driver modules already unloaded have been reloaded; the loaded driver is {current}.\naction|Select the original driver {original} in Local drivers and update.",
  },
  "notice.restore_stopped.unload_refused.reload_failed": {
    zh: "error|title|更新失败,原版驱动恢复已中止\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 时卸载驱动模块 {module} 被系统拒绝:{unload_reason},恢复在写入原版驱动文件之前中止。重新加载驱动模块失败:{reload_reason}。正式路径上的驱动文件仍为 {target}。已卸载的驱动模块没有全部重新加载,申请了显卡的应用可能无法启动。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver was stopped\nbody|Driver {target} failed the post-update check: {check_reason}. Unloading the driver module {module} was refused when restoring the original driver {original}: {unload_reason}, so the restore stopped before the original driver file was written. Reloading the driver modules failed: {reload_reason}. The driver file at the permanent path is still {target}. Not all of the unloaded driver modules were reloaded; applications that requested the GPU may fail to start.\naction|Select the original driver {original} in Local drivers and update.",
  },
  // An empty application list keeps only the action for choosing an original driver.
  "notice.restore_stopped.empty_list": {
    zh: "error|title|更新失败,原版驱动恢复已中止\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 时显卡正被占用,恢复在卸载驱动模块之前中止,未卸载任何驱动模块。当前加载的驱动为 {current}。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver was stopped\nbody|Driver {target} failed the post-update check: {check_reason}. The GPU was in use when restoring the original driver {original}, so the restore stopped before any driver module was unloaded. The loaded driver is {current}.\naction|Select the original driver {original} in Local drivers and update.",
  },
  "notice.restore_stopped.unreadable.empty_list": {
    zh: "error|title|更新失败,原版驱动恢复已中止\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 时无法读取显卡占用状态:{read_reason},恢复在卸载驱动模块之前中止,未卸载任何驱动模块。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver was stopped\nbody|Driver {target} failed the post-update check: {check_reason}. The GPU usage state could not be read when restoring the original driver {original}: {read_reason}, so the restore stopped before any driver module was unloaded.\naction|Select the original driver {original} in Local drivers and update.",
  },
  "notice.restore_stopped.unload_refused.not_unloaded": {
    zh: "error|title|更新失败,原版驱动恢复已中止\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 时卸载驱动模块 {module} 被系统拒绝:{unload_reason},恢复在写入原版驱动文件之前中止。当前加载的驱动为 {current}。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver was stopped\nbody|Driver {target} failed the post-update check: {check_reason}. Unloading the driver module {module} was refused when restoring the original driver {original}: {unload_reason}, so the restore stopped before the original driver file was written. The loaded driver is {current}.\naction|Select the original driver {original} in Local drivers and update.",
  },

  "notice.restore_failed": {
    zh: "error|title|更新失败,原版驱动恢复失败\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 失败:{restore_reason}。两次失败的详细记录见任务详情。",
    en: "error|title|Update failed; restoring the original driver failed\nbody|Driver {target} failed the post-update check: {check_reason}. Restoring the original driver {original} failed: {restore_reason}. See the task details for both failures.",
  },
  "notice.restore_failed.before_write": {
    zh: "error|title|更新失败,原版驱动恢复失败\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 失败:{restore_reason}。两次失败的详细记录见任务详情。已重新加载已卸载的驱动模块。正式路径上的驱动文件仍为 {target}。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver failed\nbody|Driver {target} failed the post-update check: {check_reason}. Restoring the original driver {original} failed: {restore_reason}. See the task details for both failures. The driver modules already unloaded have been reloaded. The driver file at the permanent path is still {target}.\naction|Select the original driver {original} in Local drivers and update.",
  },
  "notice.restore_failed.before_write.reload_failed": {
    zh: "error|title|更新失败,原版驱动恢复失败\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 失败:{restore_reason}。两次失败的详细记录见任务详情。重新加载驱动模块失败:{reload_reason}。正式路径上的驱动文件仍为 {target}。已卸载的驱动模块没有全部重新加载,申请了显卡的应用可能无法启动。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver failed\nbody|Driver {target} failed the post-update check: {check_reason}. Restoring the original driver {original} failed: {restore_reason}. See the task details for both failures. Reloading the driver modules failed: {reload_reason}. The driver file at the permanent path is still {target}. Not all of the unloaded driver modules were reloaded; applications that requested the GPU may fail to start.\naction|Select the original driver {original} in Local drivers and update.",
  },
  "notice.restore_failed.written": {
    zh: "error|title|更新失败,原版驱动恢复失败\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 失败:{restore_reason}。两次失败的详细记录见任务详情。\naction|请选择其他官网版本,或在「本地驱动」中选择一个驱动,重新更新。",
    en: "error|title|Update failed; restoring the original driver failed\nbody|Driver {target} failed the post-update check: {check_reason}. Restoring the original driver {original} failed: {restore_reason}. See the task details for both failures.\naction|Select another official version, or select a driver in Local drivers, and update again.",
  },
  "notice.restore_failed.before_write.not_unloaded": {
    zh: "error|title|更新失败,原版驱动恢复失败\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 失败:{restore_reason}。两次失败的详细记录见任务详情。正式路径上的驱动文件仍为 {target}。\naction|请在「本地驱动」中选择原版驱动 {original} 进行更新。",
    en: "error|title|Update failed; restoring the original driver failed\nbody|Driver {target} failed the post-update check: {check_reason}. Restoring the original driver {original} failed: {restore_reason}. See the task details for both failures. The driver file at the permanent path is still {target}.\naction|Select the original driver {original} in Local drivers and update.",
  },
  "notice.restore_failed.written.reload_failed": {
    zh: "error|title|更新失败,原版驱动恢复失败\nbody|驱动 {target} 未通过成功检查:{check_reason}。恢复原版驱动 {original} 失败:{restore_reason}。两次失败的详细记录见任务详情。\naction|请选择其他官网版本,或在「本地驱动」中选择一个驱动,重新更新。",
    en: "error|title|Update failed; restoring the original driver failed\nbody|Driver {target} failed the post-update check: {check_reason}. Restoring the original driver {original} failed: {restore_reason}. See the task details for both failures.\naction|Select another official version, or select a driver in Local drivers, and update again.",
  },

  "notice.no_restore": {
    zh: "error|title|更新失败,无法自动恢复\nbody|驱动 {target} 未通过成功检查:{check_reason}。无法自动恢复:{no_original_reason}。正式路径上的驱动文件仍为 {target}。\naction|请选择其他官网版本,或在「本地驱动」中选择一个驱动,重新更新。",
    en: "error|title|Update failed; automatic restore not possible\nbody|Driver {target} failed the post-update check: {check_reason}. Automatic restore is not possible: {no_original_reason}. The driver file at the permanent path is still {target}.\naction|Select another official version, or select a driver in Local drivers, and update again.",
  },

  "download.progress": {
    zh: "info|正在下载 {done} MB / {total} MB",
    en: "info|Downloading {done} MB of {total} MB",
  },

  "update.disabled_task_running_text": {
    zh: "有未结束的任务,结束后才能开始新的更新。",
    en: "A task is still running. A new update can start after it finishes.",
  },

  "extension.missing": {
    zh: "error|未检测到 TrueNAS 的 NVIDIA 驱动扩展。请先在 TrueNAS 的 Apps 设置中启用 NVIDIA 驱动。",
    en: "error|TrueNAS NVIDIA driver extension not found. Enable the NVIDIA driver in the TrueNAS Apps settings first.",
  },

  "driver.kernel_mismatch": {
    zh: "info|不可用:此驱动为内核 {target_kernel} 构建,当前内核为 {current_kernel}。如需使用此版本,请在「更新驱动」页的下拉框中选择该版本(如已列出),或重新上传该版本,然后更新;更新时会为当前内核重新构建。当前主机上已知构建不了的分支,不在这个下拉框里列出。重新上传这些分支的官方安装包,同样无法构建。",
    en: "info|Unavailable: built for kernel {target_kernel}; the current kernel is {current_kernel}. To use this version, select it in the list on the Update driver page (if it is listed) or upload it again, then update; it is rebuilt for the current kernel during the update. Branches known not to build on this host are not listed there. The official installers of these branches also fail to build when uploaded again.",
  },
  "driver.kernel_mismatch.upload": {
    zh: "info|不可用:此驱动为内核 {target_kernel} 构建,当前内核为 {current_kernel}。如需使用此驱动,请重新上传该驱动,然后更新;更新时会为当前内核重新构建。当前主机上已知构建不了的分支,不在「更新驱动」页的下拉框里列出。重新上传这些分支的官方安装包,同样无法构建。",
    en: "info|Unavailable: built for kernel {target_kernel}; the current kernel is {current_kernel}. To use this driver, upload it again, then update; it is rebuilt for the current kernel during the update. Branches known not to build on this host are not listed on the Update driver page. The official installers of these branches also fail to build when uploaded again.",
  },
  "driver.kernel_mismatch.original": {
    zh: "info|不可用:此原版驱动对应内核 {target_kernel},当前内核为 {current_kernel}。",
    en: "info|Unavailable: this original driver is for kernel {target_kernel}; the current kernel is {current_kernel}.",
  },

  "original.already_saved": {
    zh: "info|当前内核与 TrueNAS 版本的原版驱动 {original} 已保存(保存于 {date})。",
    en: "info|The original driver {original} for the current kernel and TrueNAS version is already saved (saved {date}).",
  },

  "original.skip_warning": {
    zh: "warning|跳过保存后,如本次更新失败,将没有原版驱动可供恢复,程序只会报告失败并停止。",
    en: "warning|If you skip saving and this update fails, there will be no original driver to restore; the update will only report the failure and stop.",
  },

  "catalog.help": {
    zh: "info|当前主机上已知构建不了的分支,不在这个下拉框里列出。不确定选哪个版本时,请打开旁边的 NVIDIA 官网链接,查看标注为 “Latest Production Branch” 的版本,然后在下拉框中选择这个版本(如已列出)。",
    en: "info|Branches known not to build on this host are not listed here. If you are unsure which version to choose, open the NVIDIA website link next to this list to see which version is marked “Latest Production Branch”, then choose that version in this list (if it is listed).",
  },
  "catalog.help_upload": {
    zh: "info|如需特定小版本,请从官网下载后上传。",
    en: "info|For a specific minor version, download it from the NVIDIA website and upload it.",
  },

  "upload.help": {
    zh: "info|请从 [[NVIDIA 官网|https://www.nvidia.com/en-us/drivers/unix/#linux64]]的 Linux x86_64/AMD64/EM64T 栏下载 `NVIDIA-Linux-x86_64-<版本>.run`,并同时上传 NVIDIA Container Toolkit 的 4 个 `.deb` 包(4 个包的版本须相同)。社区修改版驱动同样从此处上传。",
    en: "info|Download `NVIDIA-Linux-x86_64-<version>.run` from the Linux x86_64/AMD64/EM64T section of the [[NVIDIA website|https://www.nvidia.com/en-us/drivers/unix/#linux64]], and upload it together with the four NVIDIA Container Toolkit `.deb` packages (all four of the same version). Community-modified drivers are uploaded here as well.",
  },

  "setting.unknown": {
    zh: "error|无法识别的设置项:{variable}。请在 compose 中删除或更正该项,然后重启 App。",
    en: "error|Unrecognized setting: {variable}. Remove or correct it in the compose file, then restart the app.",
  },

  "credentials.initial_password_unusable": {
    zh: "error|初始管理员密码未设置或不符合要求(至少 8 个字符,不超过 1024 字节,不能全是空白,不能以 `<` 开头并以 `>` 结尾)。请在 compose 中设置 NVM_INITIAL_ADMIN_PASSWORD 后重启 App。",
    en: "error|The initial administrator password is missing or invalid (at least 8 characters, at most 1024 bytes, not all whitespace, not both starting with `<` and ending with `>`). Set NVM_INITIAL_ADMIN_PASSWORD in the compose file and restart the app.",
  },

  "credentials.unreadable": {
    zh: "error|无法读取凭证文件:{reason}。无法验证登录。请按下方的步骤重置账号。",
    en: "error|The credentials file could not be read: {reason}. Sign-in cannot be verified. Reset the account with the steps below.",
  },
  "credentials.unreadable.file_unreadable": {
    zh: "error|无法读取凭证文件。无法验证登录。请按下方的步骤重置账号。",
    en: "error|The credentials file could not be read. Sign-in cannot be verified. Reset the account with the steps below.",
  },
  "credentials.unreadable.directory_unreadable": {
    zh: "error|无法读取凭证文件:{reason}。无法验证登录。",
    en: "error|The credentials file could not be read: {reason}. Sign-in cannot be verified.",
  },
  "credentials.reason.not_json": {
    zh: "文件内容不是合法的 JSON",
    en: "the file is not valid JSON",
  },
  "credentials.reason.field_invalid": {
    zh: "文件字段缺失或不合法",
    en: "a field is missing or invalid",
  },
  "credentials.reason.directory_unreadable": {
    zh: "凭证文件所在目录无法读取",
    en: "the directory of the credentials file cannot be read",
  },

  "query.failed": {
    zh: "error|{query_object}查询失败:{reason}。",
    en: "error|{query_object} query failed: {reason}.",
  },
  "query.object.host_task_unit": { zh: "主机任务单元", en: "Host task unit" },
  "query.object.docker": { zh: "Docker 容器", en: "Docker container" },
  "query.object.incus": { zh: "Incus 容器", en: "Incus container" },
  "query.object.running_task": { zh: "未结束的任务", en: "Running task" },
  "query.object.extension_link": { zh: "驱动扩展链接", en: "Driver extension link" },
  "query.object.local_checksum": { zh: "本地驱动文件校验值", en: "Local driver file checksum" },
  "query.object.local_target_kernel": { zh: "本地驱动目标内核", en: "Local driver target kernel" },
  "query.object.current_kernel": { zh: "当前内核", en: "Current kernel" },
  "query.object.truenas_version": { zh: "TrueNAS 版本", en: "TrueNAS version" },
  "query.object.catalog": { zh: "NVIDIA 版本目录", en: "NVIDIA version index" },
  "query.object.kernel_headers": { zh: "内核头文件", en: "Kernel headers" },
  "query.object.gpus": { zh: "本机显卡", en: "Host GPUs" },

  "help.reset_account": {
    zh: "info|title|无法登录时重置账号\nbody|可按以下步骤删除凭证文件,由程序重建管理员账号。\naction|1. 请确认 compose 中的 NVM_INITIAL_ADMIN_PASSWORD 仍在且符合要求;如有修改,改后请重启 App。\naction|2. 请在 TrueNAS Shell 中删除数据目录下的 `manager/secrets/web-credentials.json`。数据目录是 compose 的 `volumes:` 中映射到 `/var/lib/nvm` 的主机目录。\naction|3. 程序重建账号后,请用初始管理员密码登录,并按要求修改密码。重建会清除已登记的 SSH 公钥,之后需要重新登记。",
    en: "info|title|Resetting the account if you cannot sign in\nbody|With the steps below, you delete the credentials file and the program recreates the administrator account.\naction|1. Make sure NVM_INITIAL_ADMIN_PASSWORD is still set and valid in the compose file; if you change it, restart the app afterwards.\naction|2. In the TrueNAS Shell, delete `manager/secrets/web-credentials.json` under the data directory. The data directory is the host directory mapped to `/var/lib/nvm` under `volumes:` in the compose file.\naction|3. After the program recreates the account, sign in with the initial administrator password and change the password when asked. Recreating the account clears the registered SSH public keys, which then have to be registered again.",
  },

  "task.state_unreadable": {
    zh: "error|任务状态文件无法读取:{reason}。",
    en: "error|The task state file could not be read: {reason}.",
  },

  "plan.uncomputable": {
    zh: "error|title|无法开始更新:无法计算更新计划\nbody|驱动文件未改动。\nlist|{item}",
    en: "error|title|Cannot start the update: the update plan could not be computed\nbody|The driver file was not changed.\nlist|{item}",
  },

  "precheck.failed": {
    zh: "error|title|无法更新:更新前检查未通过\nbody|以下检查项未通过,驱动文件未改动。\nlist|{item}",
    en: "error|title|Cannot update: pre-update checks did not pass\nbody|The checks below did not pass. The driver file was not changed.\nlist|{item}",
  },
  "precheck.item.link_elsewhere": {
    zh: "驱动扩展链接 `/run/extensions/nvidia.raw` 指向 {link_target},不是正式路径 `/usr/share/truenas/sysext-extensions/nvidia.raw`。",
    en: "The driver extension link `/run/extensions/nvidia.raw` points to {link_target}, not the permanent path `/usr/share/truenas/sysext-extensions/nvidia.raw`.",
  },
  "precheck.item.local_checksum_mismatch": {
    zh: "所选本地驱动文件的校验值与记录值不符。",
    en: "The checksum of the selected local driver file does not match its recorded value.",
  },
  "precheck.item.host_not_restored": {
    zh: "上一次更新对主机做的改动没有完全复位:{reason}。请重试;若仍然失败,请重启主机。",
    en: "The changes the last update made on the host were not fully put back: {reason}. Retry; if it still fails, restart the host.",
  },

  "restore.no_original": {
    zh: "error|没有可用的原版驱动:{no_original_reason}。",
    en: "error|No usable original driver: {no_original_reason}.",
  },

  "restore.two_failures": {
    zh: "error|**记录 1** 更新后的成功检查未通过:{check_reason}。\n**记录 2** 恢复原版驱动 {original} 失败:{restore_reason}。",
    en: "error|**Record 1** The post-update check failed: {check_reason}.\n**Record 2** Restoring the original driver {original} failed: {restore_reason}.",
  },

  "restore.fail.write": {
    zh: "写入原版驱动文件失败:{write_reason}",
    en: "writing the original driver file failed: {write_reason}",
  },
  "restore.fail.deadline": {
    zh: "写入原版驱动文件之前已超过执行期限({deadline} 分钟)",
    en: "the execution deadline ({deadline} minutes) passed before the original driver file was written",
  },
  "restore.fail.check": {
    zh: "恢复后的成功检查未通过:{restore_check_reason}",
    en: "the post-update check after the restore failed: {restore_check_reason}",
  },

  "startup.failed": {
    zh: "error|title|启动检查未通过\nbody|以下启动检查未通过,修正前无法开始更新。启动检查只在 App 启动时执行。\naction|修正以下各项后,请重启 App。\nlist|{item}",
    en: "error|title|Startup checks did not pass\nbody|The startup checks below did not pass; updates cannot start until they are fixed. The checks run only when the app starts.\naction|After fixing the items below, restart the app.\nlist|{item}",
  },
  "startup.pid_host": {
    zh: "error|`pid: host` 未生效,compose 中需要 `pid: host`。",
    en: "error|`pid: host` is not in effect; the compose file needs `pid: host`.",
  },
  "startup.runtime": {
    zh: "error|本容器的运行时为 {runtime},compose 中需要 `runtime: runc`。",
    en: "error|This container's runtime is {runtime}; the compose file needs `runtime: runc`.",
  },
  "startup.data_mapping": {
    zh: "error|数据目录路径映射检查未通过:{mapping_reason}。",
    en: "error|The data directory path mapping check did not pass: {mapping_reason}.",
  },
  "startup.mapping.no_hostname_line": {
    zh: "`/proc/self/mountinfo` 中没有 `/etc/hostname` 这一行,无法确定本容器",
    en: "`/proc/self/mountinfo` has no `/etc/hostname` line, so this container cannot be identified",
  },
  "startup.mapping.nsenter_failed": {
    zh: "无法进入主机命名空间(nsenter 失败),compose 可能缺少 `privileged: true`",
    en: "entering the host namespaces failed (nsenter); the compose file may be missing `privileged: true`",
  },
  "startup.mapping.inspect_failed": {
    zh: "无法读取本容器的信息(`docker inspect`):{read_reason}",
    en: "this container's information could not be read (`docker inspect`): {read_reason}",
  },
  "startup.mapping.inspect_failed.login": {
    zh: "无法读取本容器的信息(`docker inspect`)",
    en: "this container's information could not be read (`docker inspect`)",
  },
  "startup.mapping.mount_count": {
    zh: "映射到 `/var/lib/nvm` 的挂载有 {mount_count} 项,要求恰 1 项",
    en: "{mount_count} mounts are mapped to `/var/lib/nvm`; exactly one is required",
  },
  "startup.mapping.mount_under": {
    zh: "有其他挂载位于 `/var/lib/nvm/` 之下:{mount_point}",
    en: "another mount is inside `/var/lib/nvm/`: {mount_point}",
  },
  "startup.mapping.path_characters": {
    zh: "数据目录在主机上的路径含有逗号、双引号或控制字符,本程序无法把这样的路径传给主机命令",
    en: "the data directory's path on the host contains a comma, a double quote or a control character; this program cannot pass such a path to host commands",
  },
  "startup.mapping.check_write_failed": {
    zh: "无法在 `/var/lib/nvm` 中写入检查文件",
    en: "the check file could not be written in `/var/lib/nvm`",
  },
  "startup.mapping.check_read_failed": {
    zh: "无法从主机读回检查文件",
    en: "the check file could not be read back from the host",
  },
  "startup.mapping.check_mismatch": {
    zh: "从主机读回的内容与容器内写入的不同",
    en: "the content read back from the host differs from what was written in the container",
  },

  "setting.invalid": {
    zh: "error|设置项 {variable} 的值不符合要求:{requirement}。请在 compose 中更正该项,然后重启 App。",
    en: "error|The value of setting {variable} is invalid: {requirement}. Correct it in the compose file, then restart the app.",
  },
  "setting.invalid_utf8": {
    zh: "error|设置项 {variable} 的值不是合法的 UTF-8。请在 compose 中更正该项,然后重启 App。",
    en: "error|The value of setting {variable} is not valid UTF-8. Correct it in the compose file, then restart the app.",
  },
  "setting.rule.username": {
    zh: "1 到 64 个 ASCII 字符,以字母或数字开头,只含字母、数字和 `.`、`_`、`-`",
    en: "1 to 64 ASCII characters, starting with a letter or digit, and containing only letters, digits, `.`, `_` and `-`",
  },
  "setting.rule.port": {
    zh: "1 到 65535 的整数",
    en: "an integer from 1 to 65535",
  },
  "setting.rule.build_timeout": {
    zh: "1 到 1440 的整数",
    en: "an integer from 1 to 1440",
  },

  "credentials.unsaved": {
    zh: "error|凭证文件未能保存:{write_reason}。",
    en: "error|The credentials file could not be saved: {write_reason}.",
  },
  "credentials.unsaved_reason.file": {
    zh: "文件本身无法写入",
    en: "the file itself cannot be written",
  },
  "credentials.unsaved_reason.directory": {
    zh: "凭证文件所在目录无法写入",
    en: "the directory of the credentials file cannot be written",
  },

  "toolkit.below_floor": {
    zh: "warning|NVIDIA Container Toolkit {toolkit_version} 低于安全下限 1.19.1。本程序不因此拒收。",
    en: "warning|NVIDIA Container Toolkit {toolkit_version} is below the security baseline 1.19.1. This program does not reject it for this reason.",
  },

  "toolkit.help": {
    zh: "info|请同时上传 NVIDIA Container Toolkit 的 4 个 `.deb` 包:`libnvidia-container1`、`libnvidia-container-tools`、`nvidia-container-toolkit-base`、`nvidia-container-toolkit`。文件名形如 `<包名>_<版本>_amd64.deb`,4 个包的版本须相同。驱动为 615 及以上版本时,toolkit 版本须不低于 1.20.1。每个包的下载地址是 `https://nvidia.github.io/libnvidia-container/stable/deb/amd64/<包名>_<版本>_amd64.deb`。地址里的 `<版本>` 填 `https://nvidia.github.io/libnvidia-container/stable/deb/amd64/Packages` 里这个包的 Version 值。例如 `nvidia-container-toolkit` 1.20.1-1 版的下载地址是 `https://nvidia.github.io/libnvidia-container/stable/deb/amd64/nvidia-container-toolkit_1.20.1-1_amd64.deb`。",
    en: "info|Upload the four NVIDIA Container Toolkit `.deb` packages together: `libnvidia-container1`, `libnvidia-container-tools`, `nvidia-container-toolkit-base` and `nvidia-container-toolkit`. File names look like `<package>_<version>_amd64.deb`, and all four must be the same version. For drivers 615 and later, the toolkit version must be 1.20.1 or later. Download each package from `https://nvidia.github.io/libnvidia-container/stable/deb/amd64/<package>_<version>_amd64.deb`. For `<version>`, use the package's Version value in `https://nvidia.github.io/libnvidia-container/stable/deb/amd64/Packages`. For example, version 1.20.1-1 of `nvidia-container-toolkit` is at `https://nvidia.github.io/libnvidia-container/stable/deb/amd64/nvidia-container-toolkit_1.20.1-1_amd64.deb`.",
  },

  "upload.rejected.too_large": {
    zh: "error|上传已停止:{file_name} 超过 1 GiB 上限。已写入的部分已删除。",
    en: "error|Upload stopped: {file_name} exceeds the 1 GiB limit. The part already written has been deleted.",
  },
  "upload.rejected.run_not_shebang": {
    zh: "error|已拒收:{file_name} 与 `.run` 安装包的格式不符:文件头不是 `#!`。",
    en: "error|Rejected: {file_name} is not in the format of a `.run` installer: its header is not `#!`.",
  },
  "upload.rejected.header_unreadable": {
    zh: "error|已拒收:{file_name} 与 `.run` 安装包的格式不符:无法读取文件头。",
    en: "error|Rejected: {file_name} is not in the format of a `.run` installer: its header could not be read.",
  },
  "upload.rejected.run_not_linux_x86_64": {
    zh: "error|已拒收:{file_name} 的文件头中没有 `Linux-x86_64`。本程序只接受 Linux x86_64 驱动安装包。",
    en: "error|Rejected: the header of {file_name} does not contain `Linux-x86_64`. This program accepts only Linux x86_64 driver installers.",
  },
  "upload.rejected.write_failed": {
    zh: "error|上传失败:无法写入数据目录。已写入的部分已删除。",
    en: "error|Upload failed: the data directory could not be written. The part already written has been deleted.",
  },
  "upload.rejected.not_run_or_deb": {
    zh: "error|已拒收:{file_name} 不是 `.run` 或 `.deb` 文件。",
    en: "error|Rejected: {file_name} is not a `.run` or `.deb` file.",
  },
  "upload.rejected.deb_fields_unreadable": {
    zh: "error|已拒收:无法读取 {file_name} 的软件包信息。",
    en: "error|Rejected: the package information of {file_name} could not be read.",
  },
  "upload.rejected.deb_package_unknown": {
    zh: "error|已拒收:{file_name} 的包名为 {package},不是 NVIDIA Container Toolkit 的 4 个包之一。",
    en: "error|Rejected: {file_name} is the package {package}, which is not one of the four NVIDIA Container Toolkit packages.",
  },
  "upload.rejected.deb_arch": {
    zh: "error|已拒收:{file_name} 的架构为 {architecture},需要 `amd64`。",
    en: "error|Rejected: {file_name} is built for {architecture}; `amd64` is required.",
  },
  "upload.rejected.deb_version_format": {
    zh: "error|已拒收:{file_name} 的版本号 {version} 不是点分数字(可带 Debian 修订号,如 `1.20.1-1`)。",
    en: "error|Rejected: the version {version} of {file_name} is not a dotted number (a Debian revision such as `1.20.1-1` is allowed).",
  },

  "delete.confirm": {
    zh: "title|删除本地驱动\nbody|将删除以下 {row_count} 个本地驱动。只删除它们在数据目录中的文件,正式路径上的驱动文件不受影响。\nlist|{row}",
    en: "title|Delete local drivers\nbody|The {row_count} local drivers below will be deleted. Only their files in the data directory are removed; the driver file at the permanent path is not affected.\nlist|{row}",
  },
  "delete.confirm.one": {
    zh: "title|删除本地驱动\nbody|将删除以下 1 个本地驱动。只删除它们在数据目录中的文件,正式路径上的驱动文件不受影响。\nlist|{row}",
    en: "title|Delete local drivers\nbody|The local driver below will be deleted. Only their files in the data directory are removed; the driver file at the permanent path is not affected.\nlist|{row}",
  },
  "delete.confirm.row": {
    zh: "{version} · {source} · {target_kernel} · {date}",
    en: "{version} · {source} · {target_kernel} · {date}",
  },
  "date.unknown": { zh: "UNKNOWN", en: "UNKNOWN" },
};

MESSAGES["update.disabled_extension"] = MESSAGES["extension.missing"];
MESSAGES["update.disabled_task_running"] = MESSAGES["update.disabled_task_running_text"];
MESSAGES["update.disabled_password"] = MESSAGES["auth.password_change_required"];
MESSAGES["task.running"] = MESSAGES["update.disabled_task_running_text"];
MESSAGES["upload.busy"] = MESSAGES["upload.busy.text"];
MESSAGES["plan.source_invalid.toolkit_too_old"] = MESSAGES["upload.group.toolkit_too_old"];
MESSAGES["drivers.delete_disabled_task"] = MESSAGES["delete.task_running"];
MESSAGES["drivers.delete_disabled_task_unknown"] = MESSAGES["delete.task_state_unknown"];
MESSAGES["drivers.delete_disabled_canonical"] = MESSAGES["delete.canonical_unreadable"];
MESSAGES["password.unsaved"] = MESSAGES["credentials.unsaved"];
MESSAGES["login.credentials_unusable"] = MESSAGES["credentials.unreadable"];
MESSAGES["login.credentials_unsaved"] = MESSAGES["credentials.unsaved"];
MESSAGES["upload.rejected"] = MESSAGES["upload.rejected.too_large"];
MESSAGES["plan.source_invalid.gpu_unsupported"] = MESSAGES["catalog.no_usable_version"];
MESSAGES["plan.source_invalid.gpu_undecidable"] = MESSAGES["query.failed"];

MESSAGES["notice.restore_stopped.busy"] = MESSAGES["notice.restore_stopped"];
MESSAGES["notice.restore_stopped.busy.empty_list"] = MESSAGES["notice.restore_stopped.empty_list"];
MESSAGES["notice.gpu_in_use.busy"] = MESSAGES["notice.gpu_in_use"];

MESSAGES["plan.precheck_failed"] = MESSAGES["precheck.failed"];

export function messageKeys(): string[] {
  return Object.keys(MESSAGES).sort();
}

/**
 * The catalog entry for `key`, or undefined. Only the catalog's own keys
 * count: a reason code such as `constructor` or `__proto__` must not resolve
 * to an inherited object property.
 */
export function message(key: string): Message | undefined {
  return Object.hasOwn(MESSAGES, key) ? MESSAGES[key] : undefined;
}
