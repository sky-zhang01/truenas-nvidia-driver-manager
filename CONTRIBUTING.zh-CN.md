# 贡献指南

[English](CONTRIBUTING.md)

## Issue 与 PR

欢迎错误报告、文档修正及范围明确的 PR。请附应用版本、TrueNAS 版本、期望行为、实际行为与复现步骤。驱动问题另附所选来源、GPU 型号、内核版本、任务状态和脱敏后的检查结果。不要附凭证、会话令牌、私钥；原始主机资料须先检查。安全问题按 [SECURITY.zh-CN.md](SECURITY.zh-CN.md) 私下报告。

公开 `main` 由维护者从上游开发仓库导出发布。被接受的改动先由维护者合入上游，再随下一次公开导出发布。**公开 PR 不会直接合并**到公开 `main`，关闭 PR 本身不代表拒绝改动。较大的变更请先开 issue 讨论，保持修改范围集中，行为变化时同步更新中英文文档。

## 本地开发

使用 Rust **1.87+**、Node.js **24**、npm、Git、Bash 和 Python 3。Linux 开发环境能覆盖更完整的主机工具测试，部分测试在 `dpkg-deb` 可用时调用它。后端测试通过模拟 TrueNAS 主机运行，不管理真实 NAS，但也会使用本地临时文件和进程。不要在生产 NAS 上启动开发服务或运行测试。

在仓库根目录执行：

```bash
cargo test --locked
npm --prefix web ci
npm --prefix web run build
npm --prefix web run test:unit
```

以上命令依次运行后端测试、安装前端开发依赖、生成 `web/dist` 静态资源并执行前端单元测试。单元测试也将 `docs/agent.*` 的 API 与警告文字绑定到源码；相关行为变化时请同步更新说明。

端到端测试先完成上述前端构建，再执行：

```bash
cargo build --locked --release --example fake_host_server
(cd web && npx playwright install --with-deps chromium)
export NVM_E2E_DATA_DIR="$(mktemp -d)"
CI=1 npm --prefix web run e2e
```

Playwright 自动在 **18109** 端口启动 `fake_host_server`，提供 `web/dist`。release 编译可能超过 Playwright webServer 的 120 秒启动等待，预先构建 release 示例可避免因此超时。`CI=1` 要求启动新服务，不复用已有服务。模拟主机状态可变，因此浏览器测试只使用一个 worker。Playwright 虽经 localhost 访问，测试服务实际监听 **`0.0.0.0`**，并暴露没有生产认证的测试控制接口，只能在隔离开发环境运行。`NVM_E2E_DATA_DIR` 必须存在，用于保存临时测试文件。

用 Docker 构建本地应用镜像：

```bash
docker build --platform linux/amd64 -t truenas-nvidia-driver-manager:local .
```

对这个镜像运行容器测试：

```bash
NVM_IMAGE=truenas-nvidia-driver-manager:local NVM_CURL_IMAGE=<带 curl 的镜像> bash ci/container/test.sh
```

脚本在当前 Docker 上按 `deploy/compose.yaml` 启动镜像。启动自检把守护进程所在 PID 命名空间的 1 号进程当作 TrueNAS 主机；rootful Docker 下这就是本机，按原样检查。rootless Docker（普通的 rootless 安装或 rootless 的 Docker-in-Docker）都要让 RootlessKit 另开 PID、UTS 和 IPC 命名空间（`DOCKERD_ROOTLESS_ROOTLESSKIT_FLAGS="--pidns --utsns --ipcns"`）；测试开始前，脚本会在该守护进程的命名空间里补上 `/usr/bin/docker` 和 `/var/run/docker.sock`，此时 curl 镜像还须带 `nsenter`。

除下面的虚拟机测试外，不要为了测试而部署特权 App。修改 Compose 后运行 `bash ci/check-compose-test.sh`，有 Docker Compose 时会执行全部渲染配置校验。用 `bash ci/guards.sh` 扫描 tracked 文件中的坐标，新增文件也须自行检查。这些检查不能代替对日志和 fixture 的敏感信息审查。

## 在 TrueNAS 虚拟机里测试显卡解锁

`tests/gpu_unlock_vm.rs` 把本程序作为应用部署进一台 TrueNAS 测试虚拟机，用程序自己的任务跑带显卡解锁的更新，核对构建与试装、kmod 实际采用的加载顺序、重启、GPU 隔离，以及换回保存的原版。判据是 `tests/fixtures/gpu-unlock-vm.tsv` 的各行；每行的 case 一栏写明它在配了 GPU 隔离、没配、两种情况各一次，还是整台虚拟机只跑一次。虚拟机里没有 NVIDIA 显卡：生成计划时，测试把 `/sys/bus/pci/devices` 的一份副本挂在原处，多加一项 `0000:00:1f.7` 上的 RTX 3090，程序于是给这个地址规划解锁，而内核在这个地址上没有设备。

测试需要一台可以随时重装的 TrueNAS 25.10 或 27 虚拟机：已建存储池、已启用应用、能上外网，并有一个可以免密码 sudo 的 SSH 账号。测试会删掉并重建它自己的应用和数据目录，替换虚拟机的 NVIDIA 驱动，重启虚拟机，并把 NVIDIA 开关关掉再打开。不要对正在使用的 NAS 运行。先构建镜像、载入虚拟机，再在仓库根目录运行：

```bash
docker build --platform linux/amd64 -t truenas-nvidia-driver-manager:local .
docker save truenas-nvidia-driver-manager:local | ssh <vm> sudo docker load
NVM_VM_SSH='ssh -o BatchMode=yes -o ServerAliveInterval=10 <vm>' \
  NVM_VM_IMAGE=truenas-nvidia-driver-manager:local \
  NVM_VM_DATA=/mnt/<pool>/nvm-vm-test NVM_VM_OUT="$HOME/nvm-vm-evidence" \
  cargo test --locked --test gpu_unlock_vm -- --ignored --nocapture
```

`NVM_VM_DATA` 必须是普通目录而不是数据集，并且尚不存在，或是这个测试以前建的。每个 TrueNAS 版本各跑一次。一次包含三次构建、九个任务、四次重启，在测试虚拟机上用了 15 到 20 分钟。`NVM_VM_OUT` 里存每个任务的记录、每项判据所依据的读数和 `results.tsv`。

## 撰写更新记录

`CHANGELOG.md` 和 `CHANGELOG.zh-CN.md` 写给安装和使用的人看。

- 一条只写一件用户能看到、或必须做的事。用户必须先做的事放最前面。
- 不写缩写、哈希、检查数量、阶段名、重试间隔和接口字段名。没有验证过的事，直接说没有验证。
- 中文按中文的习惯写，不逐字翻译英文。
- 只有开发者关心的改动，合成最后一条，指向 `docs/agent.*`。每个版本的英文段落同时是 GitHub Release 的正文，所以单独读也要读得通。
- 先把事实分成「用户能看到」「用户必须做」「只有开发者需要」三类，再删减，最后才润色文字。
