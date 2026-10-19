# 部署说明

[English](deploy.en.md)

实测平台仅为 **TrueNAS SCALE 25.10.7 / amd64**，其他版本与架构未经测试。本 App 使用特权容器及 `pid: host` 控制主机驱动。安装前请读 [安全说明](security.zh-CN.md)，安排维护窗口并准备主机控制台；替换可能中断 GPU 工作负载。

## 1. 安装 App

1. 在 TrueNAS 打开 **应用 → 探索应用**，在 **自定义应用** 旁点 ⋮ **更多选项** → **通过YAML进行安装**，粘贴 [`deploy/compose.yaml`](../deploy/compose.yaml)。
2. 保留 `image: ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest`，或选择当前发行版对应的已发布版本。公开 GHCR 镜像无需镜像仓库登录凭据。
3. 将 `/mnt/<pool>/<dataset>` 换成专用主机目录或数据集，容器侧保留 `/var/lib/nvm`。目录下不要挂子数据集：替换使用的重命名必须在同一文件系统内。数据集须允许执行（`exec=on`）。路径不能含 `:`、`,`、`"` 或控制字符。
4. 只启用一种网络模式，再保存 App；默认是 bridge。YAML 不含密码，读取设置码后再到管理器页面设定。

可选项为 `NVM_INITIAL_ADMIN_USERNAME=admin`、`NVM_PORT=8199` 和 `NVM_BUILD_TIMEOUT_MINUTES=30`。改 `NVM_PORT` 时，端口映射两侧都要改成该端口，例如 `18199:18199`。TrueNAS 保存时会去掉注释，请保留这些说明。

## 2. 访问与登录

- 默认 bridge：从可信管理局域网打开 `http://<nas-host>:8199`。服务在容器内监听 `0.0.0.0`，模板将端口发布到 NAS 所有接口；请用网络控制限制访问，不要转发到公网。
- macvlan 或 ipvlan：删除 bridge 的端口映射，只启用其中一种模式的服务段及文件末尾段，填写网卡、子网、网关与容器地址。从局域网另一台机器打开 `http://<container-static-ip>:8199`。macvlan 可设独立 `mac_address`；ipvlan 共用主机网卡 MAC。
- 程序提供 HTTP，不提供 TLS。使用 HTTPS 反向代理时，须保留 host/origin 并保护后端连接；若需要 `Secure` cookie，请配置重写。应用即使处于 TLS 代理之后也不会自行设置该属性。

1. 在 TrueNAS 打开 **应用 → 已安装 → 这个应用**，在 **工作负载**卡片中点击容器行的文档图标，提示为**查看日志**。
2. 找到最新一条包含 `SETUP CODE:` 的 JSON 日志，在 `msg` 中选中完整的四组设置码，例如 `7KQ2-M9XD-4TWB-HE6R`。
3. 打开管理器页面，输入设置码，设定至少 8 个字符的密码并重复输入，点击**创建管理员**。管理员默认为 `admin`，也可通过 `NVM_INITIAL_ADMIN_USERNAME` 指定；创建后立即登录。

每个设置码只能使用一次，满 24 小时或 App 重启后失效。过期在请求时检查：过期后重新打开管理器页面或提交设置，日志会写入新设置码。输错不会替换仍有效的设置码。找不到日志时，请重启 App，复制最新一条包含 `SETUP CODE:` 的日志里 `msg` 中的设置码并刷新管理器页面。设置码只保存在内存中，但能读取 App 日志的人及日志收集器可以看到当前有效的设置码；请尽快完成设置，并保护 App 日志。未配置 TLS 前端时，设置码与密码通过 HTTP 传输。

以后使用管理员密码或已登记的 SSH 密钥登录。完成设置后即可登记密钥，见[智能体说明](agent.zh-CN.md)。无法登录时见[重置账号](operations.zh-CN.md)。

## 3. 替换驱动与换回原版

选择官方版本、上传可信文件，或选择可用的本地驱动。阅读生成的计划和全部警告，选择是否保存原版，再确认执行。管理器会报告 GPU 占用；需要停 App 时，由你在准备好后自行停止。

首次替换前建议保存原版驱动，跳过会失去自动恢复副本。替换后的驱动检查失败且存在可用原版时，程序才会尝试自动恢复；更早的失败并非都会触发恢复，恢复本身也可能失败。请查看最终任务和实际驱动读数，不能假定成功。

以后要换回原版，在本地驱动表选择可用的**原版**行并确认计划。此操作使用已保存文件，不下载、不构建。当前主机没有可用原版副本时，管理器无法提供该恢复。失败与中断的处置见[运维说明](operations.zh-CN.md)。

## 4. 升级或回退 App 镜像

先备份数据目录、记下当前镜像标签，并等待任务结束，再改 App。把 `image:` 改为所需的已发布 GHCR 版本并保存。跟随 `latest` 时，可在 TrueNAS 提示后使用 **更新**，也可编辑标签选择具体版本。保留相同数据挂载，账号凭证、任务记录和本地驱动都在其中。

回退应用时，把 `image:` 改回先前记录的版本标签并重新部署。这只改变管理器应用，不会恢复 NVIDIA 驱动。程序不保证较早应用能读取较新应用写入的数据，请保留升级前备份。恢复备份会丢失较新的状态；仅在没有运行任务且已另行保存仍需使用的恢复材料后进行。

## 5. 离线安装与构建

在联网的 amd64 电脑上拉取并导出公开应用镜像：

```bash
docker pull --platform linux/amd64 ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
docker save -o truenas-nvidia-driver-manager.tar ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
```

将归档传到 NAS，在**主机** shell 中导入：

```bash
docker load -i truenas-nvidia-driver-manager.tar
```

App YAML 使用导入后的仓库标签或 image ID。`docker load` 导入的归档不一定保留仓库摘要引用，不要依赖 `@sha256` 指向这个已导入的 App 镜像。运行镜像采用 `scratch`，没有 shell、发行版用户态或构建工具，App 的 **命令行** 按钮无法启动 shell。它与更大的驱动 `nvidia.raw` 镜像不同；存储规划应以实际下载产物的大小为准。

切换本地驱动、换回原版不需要构建环境。从上传安装包离线构建需要以下全部材料：

1. 固定的 amd64 Debian 构建镜像。在联网电脑上准备：

   ```bash
   set -e
   BUILD_IMAGE='debian@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587'
   docker pull --platform linux/amd64 "$BUILD_IMAGE"
   BUILD_IMAGE_ID=$(docker image inspect -f '{{.Id}}' "$BUILD_IMAGE")
   case "$BUILD_IMAGE_ID" in
     sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23|sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587) ;;
     *) printf 'Unexpected build image ID: %s\n' "$BUILD_IMAGE_ID" >&2; exit 1 ;;
   esac
   docker save -o debian-bookworm-slim-amd64.tar "$BUILD_IMAGE_ID"
   ```

   检查允许 classic 与 containerd 镜像存储使用的两个内容 ID，并按实际本地 ID 导出。将归档传到 NAS 后执行：

   ```bash
   docker load -i debian-bookworm-slim-amd64.tar
   docker image inspect -f '{{.Id}}' sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23 ||
     docker image inspect -f '{{.Id}}' sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
   ```

   第一次查找可能提示镜像不存在，此时第二次会查另一个 ID；两次都失败则停止。输出必须为 `sha256:d4c45f66efc59f89b0597d343a9bd72808493c0eacecde5285961743252cee23` 或 `sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587`，不同镜像存储可能用不同 ID 标识同一份固定内容。管理器会依次查引用及两个已知 ID，全部缺失才尝试拉取。
2. 当前版本固定的 Debian 软件包。在联网电脑的仓库根目录执行：

   ```bash
   tools/build-kit.sh fetch ./build-kit
   ```

   将所得 `.deb` 文件传到 NAS 的 `<data-directory>/manager/build-kit/`。工具和管理器会按内置清单核对文件名、字节数和 SHA-256。管理器也复用 `manager/build-kit-cache/` 中校验通过的文件。缺失或不匹配时会尝试下载，离线前请备齐。
3. 一个可信的 `NVIDIA-Linux-x86_64-<version>.run` 安装包与四个 toolkit `.deb`：`libnvidia-container1`、`libnvidia-container-tools`、`nvidia-container-toolkit`、`nvidia-container-toolkit-base`。包版本须一致，按需使用官方 `-no-compat32` 安装包或可信重构包。上传前在存放文件的电脑上计算校验和，并与可信来源比较：

   ```bash
   sha256sum NVIDIA-Linux-x86_64-*.run *.deb
   ```

   在**更新驱动**页上传这五个文件。校验和相同不等于来源可信：安装包会在构建中执行，产物会加载到主机内核。内核头文件、模块文件和内核配置来自 NAS 本身。

联网准备时，主机从 NVIDIA 下载驱动；缺少构建镜像时从 Docker Hub 拉取，从 snapshot.debian.org 下载缺少的软件包。toolkit 来自 NVIDIA 软件包仓库。在 `6.12` 内核上使用官方 `470.256.02` 驱动时，主机还会从 `deb.debian.org` 下载社区补丁归档，出站防火墙须额外放行该来源。只有 build-kit 软件包下载跟随 HTTPS 重定向（最多五次），驱动、toolkit 与社区补丁下载要求直接成功的响应。下载或校验失败会停止该阶段并记录到任务日志；修复连通性或提供核验过的离线输入，再生成新计划并确认。一次性构建容器本身不联网，也没有 CPU、内存或进程数上限。
