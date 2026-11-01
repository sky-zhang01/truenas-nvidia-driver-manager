<p>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/lockup-dark.svg">
    <img src="docs/images/lockup-light.svg" alt="TrueNAS NVIDIA Driver Manager" width="420">
  </picture>
</p>

在浏览器里选择 NVIDIA 驱动版本，确认后更新，并核对 TrueNAS 实际加载的是哪个版本。

快速跳转：[安装](#安装) · [更新驱动](#更新你的驱动) · [排障](#排障) · [安全使用](#安全使用) · [常见问题](#常见问题) · [English](README.md)

<img src="docs/images/home-landing-zh.png" width="900" alt="登录后的完整主页：页头、导航、内核里加载的驱动、显卡，以及已保存的原版驱动。">

可以选 NVIDIA 官网版本，也可以上传文件，或者用本地驱动。本地驱动是程序已经在这台机器上构建或保存过的驱动。

每次更新都要你确认。程序会检查实际加载的版本。界面上这一步叫「成功检查」，结果可能通过，也可能不通过。

第一次更新前，建议保存「原版驱动」。成功检查没通过时，程序可以拿这份尝试恢复。恢复也可能失败。

「原版」指你第一次选择保存时，正式路径（TrueNAS 存放驱动文件的固定位置）上的那份驱动文件。每个内核（TrueNAS 最底层、直接和硬件打交道的部分）和 TrueNAS 版本的组合各保存一份。程序不判断这份文件是不是 TrueNAS 提供的。

支持 TrueNAS 25.10.7（TrueNAS Community Edition）和 TrueNAS 27.0.0-RC.1（amd64，也就是 64 位的 Intel 或 AMD 电脑）。

## 安装

1. 准备 TrueNAS。

- 你的 TrueNAS 主机需要有 NVIDIA 显卡，并已启用 TrueNAS 的 NVIDIA 驱动。主页会提示驱动缺失。
  - 25.10.7：打开应用 → 已安装 → 配置 → 设置，勾选安装 NVIDIA 驱动程序，点保存。
  - 27.0.0-RC.1：打开系统 → 高级设置，在 NVIDIA Drivers 卡片点配置，勾选 Enable NVIDIA GPU Support，点保存。
- 为程序单独准备一个数据集。数据集是存储池里的一个目录。打开数据集，选中存储池，点添加数据集，起名例如 `nvm`，目录为 `/mnt/<pool>/nvm`。在高级选项里确认允许运行程序（`exec=on`）。具体要求见[部署说明](docs/deploy.zh-CN.md)。
- 主机需要能联网，访问 NVIDIA 下载站、Docker Hub 和 Debian 软件包存档。Docker Hub 是存放容器镜像的网站，构建用的镜像从这里下载。离线使用请看[部署说明](docs/deploy.zh-CN.md)。

2. 通过 YAML 安装应用。YAML 是一份纯文本配置，用来说明这个应用怎么运行。打开应用 → 探索应用，点「自定义应用」旁边的 ⋮（「更多选项」），选择「通过YAML进行安装」，应用名称随便起。粘贴下面的内容，只改数据目录。把 `<pool>` 换成你的存储池名字，把 `<dataset>` 换成数据集名字，保留后面的 `:/var/lib/nvm`，然后保存：

```yaml
services:
  nvidia-driver-manager:
    image: ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:latest
    restart: unless-stopped
    privileged: true
    pid: host
    runtime: runc
    volumes:
      - /mnt/<pool>/<dataset>:/var/lib/nvm
    ports:
      - "8199:8199"
```

> [!WARNING]
> `privileged: true` 和 `pid: host` 让应用能够控制主机。`privileged: true` 放宽应用的限制，使它可以改动主机。`pid: host` 让它看见主机上正在运行的程序。程序需要这些权限来更换主机驱动。安装前请先读[安全使用](#安全使用)。

装好以后，用主机的 8199 端口访问程序（浏览器地址里冒号后面的数字就是端口）。默认的 bridge 方式，就是用 TrueNAS 的地址加这个端口。macvlan 和 ipvlan 会给程序自己的地址。要用 macvlan、ipvlan，换端口，或离线使用，见 [`deploy/compose.yaml`](deploy/compose.yaml) 和[部署说明](docs/deploy.zh-CN.md)。TrueNAS 保存时会去掉 YAML 里的注释，这是正常的。

3. 复制设置码。等应用显示为运行中，打开应用 → 已安装 → 你的应用 → 工作负载，在容器那一行点文档图标（查看日志）。容器就是这个应用正在运行的那一份。最新的日志在最下面。日志为空时，等一分钟再打开。找最新的 `setup_code_issued` 事件，设置码在它的 `msg` 里，`SETUP CODE:` 后面。只复制那四段字符，不要带引号和句号。下面的示例省略了其他字段和部分正文，你的设置码会不同。实际日志是一整行：

```json
{ "event": "setup_code_issued", "msg": "SETUP CODE: 7KQ2-M9XD-4TWB-HE6R. ..." }
```

4. 创建管理员。在浏览器打开 `http://<nas-ip>:8199/`。`<nas-ip>` 是平时打开 TrueNAS 用的地址。用了 macvlan 的话，改用应用自己的地址。输入设置码，保留用户名 `admin`，设定至少 8 个字符的密码，再输入一次。点创建管理员，就会登录。

<img src="docs/images/setup-zh.png" width="900" alt="设置页面：输入设置码并设定管理员密码。">

*仅为示例。请到应用日志里取你的设置码，输入后设定管理员密码。*

设置码只能用一次，24 小时内有效。设置完成前如果重启应用，会生成新的设置码，请用最新的一条。设置完成后，用 `admin` 和你的密码登录，不再需要设置码。设置码被拒绝时，请看[常见问题](#常见问题)。

如果以前用过的工具会在开机时换回它自己的驱动，重启后，它可能把本程序装的驱动又换掉。本程序不改那个工具的开机设置。要不要停用那个工具，由你决定。

## 更新你的驱动

> [!WARNING]
> 换驱动会中断所有使用显卡的应用，请选在空闲时段进行，并在第一次更新前先保存原版驱动。另外请准备好直接操作主机的方式，以防显卡没有恢复。可以用 TrueNAS 的命令行（系统 → 命令行），也可以用接在主机上的键盘和显示器。

1. 打开更新驱动，勾选显卡，选择 NVIDIA 官网版本，点检查并继续。
2. 等待时停止标为「正在使用显卡」的应用，检查会自动继续。程序不会替你停止应用。
3. 选择「保存原版驱动并开始」（已经保存过原版时点「开始更新」），确认更新。
4. 等待任务完成，关掉浏览器也不影响任务继续运行。
5. 确认任务成功，并核对任务页和主页上的新驱动版本。
6. 重启驱动变更前已启动的显卡应用，包括主页列出的应用和提示「Driver/library version mismatch」的应用。

每一步的截图和详细说明见下面的更新指南。

## 更新指南

<details>
<summary>1. 选择显卡和驱动</summary>

1. 在左侧菜单里打开更新驱动（窄屏时菜单在页面底部）。如果有不止一张 NVIDIA 显卡，勾选将使用这个驱动的显卡。
2. 点开「NVIDIA 官网版本」下拉框，选一个版本。列表里是每个分支在这台机器上能构建的最新版本。分支就是驱动的一个发布系列。别的版本请选最后一项「其他版本…」，再输入版本号。
3. 拿不准时，点「NVIDIA 官网 ↗」，找标着「Latest Production Branch」的版本。下拉框里有的话，就选它。
4. 点检查并继续。

离线使用，或要用自己的文件（含社区修改版）时，点手动上传文件…，按 5 行清单准备，见[驱动来源](#驱动来源)。

Debian 社区兼容补丁只在构建官方 470.256.02 时使用。手动上传的 470 安装包不会套用这些补丁。官方 470.256.02 打过补丁的构建需要联网下载补丁归档（那组兼容补丁），不能事先放进 `manager/build-kit/`；构建软件包则可以事先放进去。

<img src="docs/images/update-zh.png" width="900" alt="更新驱动页面：选择 NVIDIA 官网版本，然后点「检查并继续」。">

</details>

<details>
<summary>2. 检查显卡使用状态</summary>

检查时会列出正在使用显卡，或正在申请显卡的应用，名单自动刷新。请停止标为「正在使用显卡」的应用，检查会自动继续。等了大约 30 秒还在使用时，结果窗口会显示名单是什么时候取的。停掉这些应用，再点「重新检查」。

</details>

<details>
<summary>3. 确认更新</summary>

在确认窗口核对来源、版本和系统。出现「保存原版驱动并开始」时点它；已经保存过原版时，点「开始更新」。

<img src="docs/images/plan-zh.png" width="900" alt="官网版本的确认更新窗口，按钮有「保存原版驱动并开始」「不保存原版驱动并开始」和「返回选择」。">

</details>

<details>
<summary>4. 跟进任务</summary>

任务页会显示当前阶段。实时日志在页面下方。同一时间只运行一个更新。关掉浏览器，任务仍会继续。下载和构建默认共用 30 分钟的时限。构建是把驱动做成这台机器能加载的文件。如果在驱动文件被替换之前超时，更新会停止，驱动文件保持不变。

<img src="docs/images/running-zh.png" width="900" alt="进行中的任务，显示当前阶段。">

</details>

<details>
<summary>5. 核验结果</summary>

任务状态为成功，更新后的检查会显示你的新版本。主页显示的版本也一致。

<img src="docs/images/success-zh.png" width="900" alt="已完成的任务，提示更新完成。">

</details>

<details>
<summary>6. 重启显卡应用</summary>

请重启驱动变更前已启动的显卡应用。主页「驱动版本已变更」列出的应用，以及提示「Driver/library version mismatch」的应用均需重启。

</details>

<details>
<summary>可选：加入显卡全速解锁（gpu-unlock）</summary>

GeForce RTX 30、40、20 系列显卡做 FP16、BF16、TF32 矩阵运算（累加用 FP32）时只有半速。这个可选功能可以解除该限制。它使用 [Zagitta/gpu-unlock](https://github.com/Zagitta/gpu-unlock) 2.0.0 的内核模块（GPL-2.0-only 许可）。速度提升只有在真实显卡上才能测出，本项目没有测过。

- 默认关闭，每次更新单独选择：在更新驱动页的版本列表下面勾选「加入显卡全速解锁（gpu-unlock）」。手动上传窗口里有同样的勾选框。
- 只有官网或上传的 610.43.03 驱动可以带上它。勾选框下面有一行「解锁支持范围」：RTX 30 系列稳定；RTX 40 系列、RTX 20 系列实验性；RTX 50 系列不支持。能不能解锁，程序会按每块显卡自动判断，不用你去对型号。所勾选的显卡都不支持时，勾选框不能勾上。确认窗口里，本次解锁的显卡每涉及一个实验性系列，就提醒一次。
- 它只作用于所勾选且支持的显卡，不会作用于为虚拟机隔离的显卡，下次开机生效。确认窗口和完成的任务都会写明。
- 离线主机：在能联网的电脑上下载 `NVIDIA-Linux-x86_64-610.43.03-no-compat32.run`，作为驱动上传，再勾选这个框，不会下载任何东西。解锁固件由你的主机从这个安装包生成，程序本身不带固件。
- 下次开机后，主页为每块解锁目标显卡显示「显卡全速解锁」读数。如果写的是需要断电后重新开机，请关机、断开电源再开机；在 TrueNAS 界面里点重启，不一定会让显卡断电。
- 想去掉解锁，更新到任何不带解锁的驱动即可：不勾选解锁的版本、不带解锁的本地驱动，或原版驱动。
- 这个模块和程序的 `nvidia.ko` 一样没有签名；系统强制检查模块签名时，两者都装不上。在 TrueNAS 设置里把 NVIDIA 驱动开关关掉再打开，会换回 TrueNAS 自己的驱动文件，解锁随之消失。

阶段、检查、失败提示和主页读数见[运维说明](docs/operations.zh-CN.md#显卡全速解锁gpu-unlock)。

</details>

## 本地驱动

<details>
<summary>本地驱动：已保存的版本与可用操作</summary>

本地驱动页列出程序构建或保存过的驱动。为当前内核构建的驱动可以直接切换，不用重新下载；为其他内核构建的会显示「内核不匹配」。要用这样的驱动：列表里有的版本，在更新驱动页再选一次；上传的驱动，重新上传一次。更新时会为当前内核重新构建。为其他内核保存的原版驱动无法使用。带解锁构建的驱动，在「来源」列里显示「· 含全速解锁」。

<img src="docs/images/drivers-zh.png" width="900" alt="本地驱动页，正在使用的驱动有标记。">

</details>

## 驱动来源

<details>
<summary>三种驱动来源</summary>

驱动可以来自三个地方：

- NVIDIA 官网版本。主机从 NVIDIA 官网下载你选的驱动。
- 你上传的文件。点手动上传文件…，按 5 行清单准备：1 个驱动安装包，4 个 toolkit 包。这 4 个 NVIDIA Container Toolkit 包让容器里的应用能用上显卡。每一行都有下载链接，在新标签页打开。推荐 `NVIDIA-Linux-x86_64-<版本>-no-compat32.run`。这个文件不含 TrueNAS 用不到的 32 位库。4 个 `.deb` 必须是同一版本的 amd64 包。`.deb` 是 Debian 的软件包格式。toolkit 的版本从 NVIDIA 官方索引读取。索引读不到时，用 `1.20.1-1`。
- 以前保存过的驱动。换成它时，不用再下载。

选好文件就会开始上传。先传 `.deb`，再传比较大的 `.run`。文件按内容归到清单的某一行。扩展名必须是 `.run` 或 `.deb`。每一行会显示进度和错误。文件还没齐时，底部显示已经就绪的数量。齐了以后，底部显示完成摘要。上传过程中可以取消，页面会先让你确认，已经收到的文件会留下。5 个都就绪后，点检查并继续。检查成功，才打开确认窗口。点「返回选择」会回到上传清单，文件还在。任务一开始，这次用到的上传文件就不能再换。指纹和校验值在确认窗口的文件详情里。它们是按文件内容算出来的值，用来核对文件是否一致。程序会检查文件内容和包的版本，构建时还会跑安装包自带的完整性检查。程序不验证 NVIDIA 签名。构建失败时，驱动文件不变。

</details>

## 排障

失败后先看主页，确认现在加载的是哪个驱动。

<details>
<summary>排错与恢复</summary>

- 更新在替换驱动文件之前就失败了。驱动文件没有改动。看任务页上的提示，排除原因后重新更新。内核模块（装进内核里的那一部分驱动）可能已经被卸掉。请看使用显卡的应用还能不能启动，不能就重新启动它们。
- 新驱动没通过检查，而且已经保存了原版驱动。程序会尝试恢复。任务显示「已恢复原版驱动」时，你这次要的更新没有成功。
- 没有保存原版。程序只能报告失败。更新前建议保存原版。
- 你想换回原版驱动。打开本地驱动，选来源为原版的那一行，点换回此版本。这一步不需要联网。
- 设置码被拒绝、已过期或找不到。请看常见问题里的[设置码被拒绝、已过期或找不到时怎么办？](#常见问题)
- 任务显示「已中断」（比如断电或主机重启）。打开主页。那里有已加载的驱动版本，以及停在哪一步。核对版本后，再更新一次。如果应用还是用不了显卡，先在本地驱动里换到原版那一项。然后在空闲时段重启主机，让 TrueNAS 重新加载当前驱动。重启仍可能无法恢复。

如果恢复失败，你需要能直接操作主机，例如 TrueNAS 的命令行，或接在主机上的键盘和显示器。每个任务页都有日志。所有情况的详细说明见[运维说明](docs/operations.zh-CN.md)。

</details>

## 安全使用

- 程序能改动你的主机。它拥有和 TrueNAS 管理员相当的权限。能登录它的人，就能控制驱动和主机。
- 请限制在可信的管理网络里使用。页面走明文 HTTP，传输的内容不加密。默认整个内网都能打开。不要把这个端口转发到互联网。
- 首次启动后，尽快完成设置。在此之前，读得到应用日志的人都能使用设置码。
- 请用强密码，并且只上传你信任的文件。上传的安装包会在你的主机上运行，并成为系统的一部分。
- 先保存原版驱动，并备份数据目录。数据目录里有你的账号、任务记录，以及保存下来的驱动，包括原版驱动。

其余内容，比如构建时占多少资源，见[安全说明](docs/security.zh-CN.md)。

## 常见问题

<details>
<summary>会自动更新吗？</summary>

不会。每次更新都要在确认窗口确认。

</details>

<details>
<summary>支持哪些系统？</summary>

支持并实测了 TrueNAS 25.10.7 与 TrueNAS 27.0.0-RC.1 / amd64，其他版本和其他处理器类型（如 ARM）都没有测试。Maxwell、Pascal、Volta 显卡在 TrueNAS 27 上尚未实测。这三个名字是 NVIDIA 的显卡世代。列表里有某个版本，不代表它一定适合你的显卡和应用，确认前请先看检查结果和警告。

</details>

<details>
<summary>哪些显卡用哪种驱动？</summary>

程序按你勾选的显卡，决定用哪一种内核模块。驱动版本为 555 及以上，并且至少勾选了一张显卡时：只要其中有显卡必须用闭源模块（包括 Maxwell、Pascal、Volta），就构建闭源模块（NVIDIA 不公开源代码），否则构建开源模块。Maxwell、Pascal、Volta 最后能用的驱动分支是 580。

Kepler 显卡（包括部分 GTX 600/700 型号）请选官方 470.256.02。Kepler 是 NVIDIA 较早的一代显卡。本程序在 TrueNAS 25.10（内核 6.12）和 TrueNAS 27（内核 6.18）上都提供这个版本。程序会下载 Debian 的社区兼容补丁并套用。补丁的套用和编译，已经在 TrueNAS 25.10.7 和 27.0.0-RC.1 的内核上测过。在真实的 Kepler 显卡上，加载、显示和 CUDA 还没有验证。CUDA 是 NVIDIA 用来在显卡上做计算的软件。470 已经停止维护，NVIDIA 不再为它发布安全修复。只建议在没有别的受支持驱动时使用。

这套 Debian 补丁会让 470 在内核打开 IBT（间接分支跟踪，CPU 的一项安全功能）时拒绝加载。补丁说明里写的是 Intel 第 11 代以及更新的 CPU。可以在 TrueNAS 的命令行（系统 → 命令行）里运行下面这条命令。命令会数出带有 `ibt` 的行数：`grep '^flags[[:space:]]*:' /proc/cpuinfo | grep -c -w ibt`。结果是 0，只表示这次没有匹配到。不能据此确认 470 能加载。没有匹配时，这条命令的退出码是 1。

</details>

<details>
<summary>版本列表为空或下载失败时怎么办？</summary>

主机要能访问 NVIDIA 下载站、Docker Hub 和 Debian 软件包存档。请先查防火墙和 DNS，再看任务日志。DNS 负责把网站名字换成地址。列表里有哪些版本，还跟你勾选的显卡有关。比较老的显卡，只会看到它能用的版本。

</details>

<details>
<summary>更新到一半停止时怎么办？</summary>

打开主页和任务页，会看到停在哪一步以及已加载的驱动版本。每个阶段的含义见[运维说明](docs/operations.zh-CN.md)。

</details>

<details>
<summary>设置码被拒绝、已过期或找不到时怎么办？</summary>

1. 刷新程序页面。设置码过期后，日志里会写入新的设置码。
2. 再次打开应用日志，复制最新一条包含 `SETUP CODE:` 的日志里的设置码。最新的日志在最下面。只复制设置码（四段字符），不要带引号。
3. 如果没有这样的日志，在应用 → 已安装里重启应用，等一分钟，再看日志。
4. 页面提示稍后再试时，按提示的秒数等待。输错设置码和输错密码共用同一个次数限制。

</details>

<details>
<summary>怎样重置账号？</summary>

1. 在 TrueNAS 里打开主机的命令行（系统 → 命令行）。程序自己的命令行按钮点了没反应，不要用它。
2. 删除数据目录里的账号文件 `web-credentials.json`。命令里的 `<数据目录>` 换成你在 YAML 里填的路径，也就是 `:/var/lib/nvm` 前面那一段。这会删除账号和已保存的 SSH 密钥，并让所有设备退出登录，驱动和任务不受影响：

   ```sh
   rm '<数据目录>/manager/secrets/web-credentials.json'
   ```

3. 刷新页面，页面会重新显示设置页，应用会把新的设置码写进日志。
4. 在 TrueNAS 打开应用 → 已安装，点你的应用，在工作负载卡片里点容器那一行的文档图标（查看日志）。复制最新一条包含 `SETUP CODE:` 的日志里的设置码。如果没有出现新的日志，再刷新一次页面；还是没有就重启应用。
5. 输入设置码，设定新密码。不需要重启应用。如果你用 SSH 密钥，需要重新登记。SSH 密钥是另一种登录方式。

详细步骤见[运维说明](docs/operations.zh-CN.md)。

</details>

<details>
<summary>为什么 TrueNAS 中应用的命令行按钮没有反应？</summary>

程序的镜像里没有命令行环境（shell），所以这个按钮没有东西可打开，这是正常的。要查原因，请看任务页。

</details>

<details>
<summary>怎样升级程序或固定版本？</summary>

先等正在运行的任务结束，再备份数据目录。使用 `latest` 标签时，打开应用 → 已安装，点你的应用，有新镜像时在应用信息卡片上选 ⋮ → 更新。想改成固定版本，就点编辑，把镜像标签改成版本号，然后保存。改应用本身，不会改变主机上的 NVIDIA 驱动。如果要换回原版驱动，请先在本地驱动里做。详情见[部署说明](docs/deploy.zh-CN.md)。删除程序的分步说明还没有写。

YAML 里的镜像标签（镜像地址里最后一个冒号后面的名字）是 `latest`。想固定版本时，改成 `0.4.0`：`ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.4.0`。

</details>

## 进阶用户

<details>
<summary>指南、智能体权限与镜像验证</summary>

- [部署说明](docs/deploy.zh-CN.md)：网络方式（bridge、macvlan、ipvlan）、应用升级与回退、离线安装与离线构建、联网下载来源。
- [运维说明](docs/operations.zh-CN.md)：换回原版驱动、排错、SSH 密钥登录、重置密码、备份、中断任务、日志。
- [安全说明](docs/security.zh-CN.md)：程序的工作方式、需要的权限、会话处理、TLS（网页连接的加密）与已知限制。
- [智能体说明](docs/agent.zh-CN.md)和[智能体技能](skills/nvidia-driver-replacement/SKILL.md)：可以让 Claude Code、Codex 这类 AI 智能体来做更新。说明会要求智能体每次替换前先问你。程序无法确认它是不是真的问了。登录之后，智能体拥有完整的管理员权限。
- [安全报告](SECURITY.zh-CN.md)、[贡献与本地开发](CONTRIBUTING.zh-CN.md)、[更新记录](CHANGELOG.zh-CN.md)。

<details>
<summary>可选：验证镜像确实来自本仓库</summary>

```bash
gh attestation verify oci://ghcr.io/sky-zhang01/truenas-nvidia-driver-manager:0.4.0 --repo sky-zhang01/truenas-nvidia-driver-manager
```

</details>

</details>

## 获取帮助

提交 issue 时请附上：程序版本、TrueNAS 和内核版本、显卡型号、驱动来源和目标版本、任务状态和任务日志。贴日志前先去掉其中的地址、路径、密码和密钥。安全问题请按 [SECURITY.zh-CN.md](SECURITY.zh-CN.md) 私下报告。

## 致谢

470 的兼容补丁来自 Debian 的 `nvidia-graphics-drivers-tesla-470` 打包（470.256.02-10），由 Debian 的 NVIDIA 维护者整理，主要作者为 Andreas Beckmann，另有数位贡献者。Debian 将这些补丁标为放弃版权的公有领域声明。程序在构建时从 snapshot.debian.org 下载并原样应用，本仓库不包含这些补丁。构建脚本的一部分取自 `biohazardious/truenas-nvidia-driver-updater`。可选的显卡解锁使用 [Zagitta/gpu-unlock](https://github.com/Zagitta/gpu-unlock) 2.0.0 的内核模块，原样放入本仓库。见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

## 许可与商标

源代码采用 MIT 许可，但 `third_party/gpu-unlock/` 里原样放入的 gpu-unlock 内核模块除外，它采用 GPL-2.0-only 许可。见 [`LICENSE`](LICENSE) 和 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。

本项目与 iXsystems 或 NVIDIA 无关联，未获其背书。TrueNAS 是 iXsystems, Inc. 的商标，NVIDIA 是 NVIDIA Corporation 的商标。标志和品牌素材（包括其中的字标）不适用 MIT 许可，权利归各自所有者。

程序的镜像不包含 NVIDIA 驱动，也不包含解锁固件。使用官方来源更新时，驱动由你的主机从 NVIDIA 下载。上传的安装包由你自己提供。解锁固件由你的主机从这个 NVIDIA 安装包生成。NVIDIA 驱动软件受 NVIDIA 许可条款约束，与本项目的 MIT 许可相互独立。
