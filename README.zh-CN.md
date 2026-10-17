# truenas-nvidia-driver-manager

在网页上为 TrueNAS SCALE 主机选择、构建、替换和恢复 NVIDIA 驱动。

[English](README.md)

可以从 NVIDIA 下载目录选择版本，上传驱动安装包及四个 `nvidia-container-toolkit` 包，或切换到已保存的本地驱动。管理器按需构建 `systemd-sysext` 镜像，按选择保存原厂驱动，替换永久路径上的镜像，并核对加载结果。替换后的驱动检查失败时，若有可用的原厂备份，就尝试恢复。本地驱动与任务日志可在界面查看。每次替换都需要人确认，没有定时更新。

## 兼容性

唯一实测平台是 **TrueNAS SCALE 25.10.7 / amd64（Linux x86_64）**。其他 TrueNAS 版本与架构均未测试。版本出现在列表中不代表适用于所有 GPU、内核和工作负载；确认前请阅读计划中的 GPU 检查与警告。

## 安全模型

- App 运行在**特权容器**中，使用 `pid: host`，经 `/proc/1/root` 读取主机，进入主机命名空间执行命令，并为每次确认的替换启动主机 systemd 单元。它可以修改主机驱动文件及卸载、加载内核模块。管理器、镜像和数据目录的访问权限应视为主机管理员权限。
- 服务默认在**容器内的 `0.0.0.0:8199` 提供明文 HTTP**。默认 bridge 映射 `8199:8199` 会将端口发布到 NAS 的所有接口；也支持 macvlan 与 ipvlan。请限制为可信管理局域网访问，不要暴露到公网。
- 只有一个管理员账号，初始口令来自 `NVM_INITIAL_ADMIN_PASSWORD`，默认用户名为 `admin`。首次登录后必须改成不同口令，才能执行改动操作。口令与 SSH 密钥登录失败共用限流。
- 已认证 API 请求必须同时携带签名会话 cookie 和该会话的 `X-NVM-Token`。只有非 `GET`/`HEAD` 请求接受 Origin 检查。会话有效期为 12 小时，cookie 带 `HttpOnly` 与 `SameSite=Strict`。两者都应保密。应用**不会**给 cookie 添加 `Secure`，部署条件见下文。
- 智能体可以用管理员登记的专用 SSH 密钥签署一次性挑战登录，管理器只接收公钥与签名。这会取得同样的管理员会话，不会开启 SSH 服务，也没有权限更低的智能体角色。删除公钥会作废已有会话。每次替换、重试和换回原厂前，智能体必须展示准确计划并取得人的确认；API 无法证明确认来自人。详见[智能体说明](docs/agent.zh-CN.md)。

## 已知限制

- 替换驱动可能中断 GPU 工作负载。管理器会报告占用者并拒绝不安全的计划，但不会代停 App。自动恢复需要已保存的原厂驱动，恢复本身也可能失败。请备份数据目录，并准备主机控制台访问方式。
- 构建使用一次性容器，禁用网络、移除全部 capability，并设置 `no-new-privileges`，但**未设置 CPU、内存或进程数上限**，可能与 NAS 服务争用资源。下载与构建默认合计共用 30 分钟时限。
- 程序不内置 TLS。HTTPS 反向代理须保留请求的 host/origin，并保护后端连接。程序不会根据转发头给 cookie 添加 `Secure`；浏览器收到的 cookie 是否带该属性，取决于部署侧是否重写。仅终止 TLS 不会添加该属性。
- 上传的安装包会在构建中执行，并生成要加载到主机内核的代码。只使用可信文件，并与可信来源核对 SHA-256。校验和只能标识字节，不能证明来源。
- 联网构建依赖 NVIDIA 下载、Docker Hub 和 Debian 快照软件包。在 `6.12` 内核上使用官方 `470.256.02` 驱动时，主机还会从 `deb.debian.org` 下载社区补丁归档，出站防火墙须额外放行该来源。下载或校验失败会停止准备；查看任务日志，解决原因后重试。离线构建必须提前准备固定的构建镜像与软件包。
- 社区补丁驱动不支持用户 I/O 内存映射，启用 IBT 时无法加载，显示热插拔行为可能不同；Kepler 的加载、显示与 CUDA 工作负载尚未验证。请遵守所选来源的警告。

## 安装与升级

在 TrueNAS 的 **Apps → Discover → Install via YAML** 中使用 [`deploy/compose.yaml`](deploy/compose.yaml)，设置专用数据目录与强初始口令。发布镜像使用以下地址：

```text
ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.2.0
```

用版本标签选择具体版本。升级时编辑 App 的镜像标签；跟随 `latest` 且 TrueNAS 提示可更新时，也可使用 **Update**。升级前备份并保留数据目录。回退 App 镜像与恢复主机驱动是两件事，见[部署说明](docs/deploy.zh-CN.md)与[运维说明](docs/operations.zh-CN.md)。

运行镜像使用 `scratch`，不含 shell 或构建工具。排查请看任务页和主机侧容器日志；App 的 **Shell** 按钮没有可启动的 shell。

## 文档与贡献

- [部署说明](docs/deploy.zh-CN.md)：安装、网络、升级和离线准备。
- [运维说明](docs/operations.zh-CN.md)：恢复原厂驱动、重置口令、备份与排错。
- [智能体说明](docs/agent.zh-CN.md)与[技能](skills/nvidia-driver-replacement/SKILL.md)。
- [安全报告](SECURITY.zh-CN.md)、[贡献与本地开发](CONTRIBUTING.zh-CN.md)、[更新记录](CHANGELOG.zh-CN.md)。

Claude Code、Codex 等智能体可从仓库根目录安装技能与英文说明。以下为 Claude Code 的目录；Codex 使用时将 `DEST` 改为 `"$HOME/.agents/skills/nvidia-driver-replacement"`。技能升级后重新执行。

```bash
DEST="$HOME/.claude/skills/nvidia-driver-replacement"
mkdir -p "$DEST/docs"
cp skills/nvidia-driver-replacement/SKILL.md "$DEST/SKILL.md"
cp docs/agent.en.md "$DEST/docs/agent.en.md"
```

## 许可与商标

项目源代码采用 MIT 许可，见 [`LICENSE`](LICENSE)；第三方声明见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。

本项目**与 iXsystems 或 NVIDIA 无关联，也未获其背书**。TrueNAS 是 iXsystems, Inc. 的商标，NVIDIA 是 NVIDIA Corporation 的商标。品牌素材及其中的字标不适用 MIT 许可，权利归各自所有者。

项目镜像不包含 NVIDIA 驱动安装包。使用官方来源更新时，由用户主机从 NVIDIA 下载；上传来源的安装包由用户提供。NVIDIA 驱动软件受 NVIDIA 适用许可条款约束，与本项目的 MIT 许可独立。
