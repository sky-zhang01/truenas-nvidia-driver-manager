# 贡献指南

[English](CONTRIBUTING.md)

## Issue 与 PR

欢迎错误报告、文档修正及范围明确的 PR。请附应用版本、TrueNAS 版本、期望行为、实际行为与复现步骤。驱动问题另附所选来源、GPU 型号、内核版本、任务状态和脱敏后的检查结果。不要附凭证、会话令牌、私钥；原始主机资料须先检查。安全问题按 [SECURITY.zh-CN.md](SECURITY.zh-CN.md) 私下报告。

公开 `main` 由维护者从上游开发仓库导出发布。被接受的改动先由维护者合入上游，再随下一次公开导出发布。**公开 PR 不会直接合并**到公开 `main`，关闭 PR 本身不代表拒绝改动。较大的变更请先开 issue 讨论，保持修改范围集中，行为变化时同步更新中英文文档。

## 本地开发

使用 Rust **1.85+**、Node.js **24**、npm、Git、Bash 和 Python 3。Linux 开发环境能覆盖更完整的主机工具测试，部分测试在 `dpkg-deb` 可用时调用它。后端测试通过模拟 TrueNAS 主机运行，不管理真实 NAS，但也会使用本地临时文件和进程。不要在生产 NAS 上启动开发服务或运行测试。

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

不要为了测试而部署特权 App。修改 Compose 后运行 `bash ci/check-compose-test.sh`，有 Docker Compose 时会执行全部渲染配置校验。用 `bash ci/guards.sh` 扫描 tracked 文件中的坐标，新增文件也须自行检查。这些检查不能代替对日志和 fixture 的敏感信息审查。
