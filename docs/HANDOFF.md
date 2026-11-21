# 云端开发交接

更新：2026-11-21。本文负责接手路径与环境；任务状态只在 [ROADMAP](ROADMAP.md#当前待办) 维护，验证结果只在 [STATUS](STATUS.md) 维护。

## 1. 先确认拿到的是哪个项目

| 项目 | 值 |
| --- | --- |
| 当前名称 / 版本 | whix / 0.1.0；2026-10-07 从 whistle-rs 更名 |
| 仓库 / 主分支 | [uxiew/whix](https://github.com/uxiew/whix) / `main` |
| 本次整理前功能源码 | `10bb71d05b6b7880f524d9802007e9fd3e477900`，最新功能为 SCRIPT-03 |
| 本轮范围 | 文档、历史报告归档、后续任务与推送；不新增代理功能 |
| 产品方向 | 本机开发调试、Rust 可嵌入核心、自有控制台与插件协议 |
| 验证来源 | [STATUS](STATUS.md) 中每一节的日期、提交与范围；不能跨快照复用结论 |

接手时执行以下只读检查，再建立自己的工作分支。本文中的源码基线用于识别历史，不要求倒退当前分支。

```sh
git status --short --branch
git remote -v
git rev-parse HEAD
git log -5 --oneline
```

这次整理从干净工作树开始，当时 `main` 比 `origin/main` 多 15 个功能/测试提交；最终交付提交及远端结果以 Git 为准。接手者不能只取本次文档 diff 而漏掉这些功能提交。

## 2. 最短阅读路径与文件归属

| 文件 / 目录 | 职责 |
| --- | --- |
| [AGENTS.md](../AGENTS.md) | 所有模型共用的仓库约定 |
| [README.md](../README.md) | 产品介绍、启动、常用规则与文档入口 |
| [ROADMAP.md](ROADMAP.md) | 当前待办、候选范围和验收；历史完成项保留原编号 |
| [STATUS.md](STATUS.md) | 当前快照和按日期追加的验证证据 |
| [DEVELOPMENT.md](DEVELOPMENT.md) | 构建、分层验证、CI 与制品流程 |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 模块与请求生命周期 |
| [RULES.md](RULES.md)、[API.md](API.md)、[PLUGINS.md](PLUGINS.md) | 对外行为与明确不支持的范围 |
| `src/rules/`、`src/proxy/`、`src/plugins/` | 规则引擎、传输/控制台 API、插件运行时 |
| `ui-src/`、`sdk/` | Vue 控制台、自有 JS/TS 插件 SDK |
| `tests/*.rs`、`tests/common/`、`tests/differential/` | Rust 真实进程夹具与固定上游差分 |
| [9 月核心审查](audits/whistle-core-parity-audit-2026-09-30.md)、[早期路线图](ROADMAP-HISTORY.md) | 历史材料；不是当前缺陷清单 |

不用先通读所有长文档。先读当前快照与待办，再读选中任务引用的源码/测试。原始 `target/` 日志不在 Git 中；缺少日志时重跑已入库测试，不凭历史数字补造证据。

## 3. 云端第一次构建

需要 Git、平台 C/链接工具链、rustup 和 Node.js。Rust 固定 **1.98.1**；MSRV **1.95** 另测。UI 要求 `^20.19.0 || >=22.12.0`，常规差分的 CI 使用 **Node 26**，建议云端对照也使用 26。

在仓库根目录顺序执行：

```sh
npm ci --prefix ui-src
npm run typecheck --prefix ui-src
npm run build --prefix ui-src
cargo build --locked
npm ci --prefix tests/differential
npm ci --prefix tests/differential/versions/2.10.10
```

`cargo build` 默认生成差分入口使用的 debug 二进制；只构建 release 不满足默认路径。运行核心不依赖 Node，但构建控制台、Node 插件和差分需要它。`sdk/` 无需单独安装依赖。不要复制本机 `target/`、`node_modules/` 或私有配置充当云端初始化。

首次需要联网获取锁定依赖；`--offline` 只用于缓存已齐备的环境。上游自带测试首次还会下载固定提交的测试目录，需要 GitHub 与 `tar`，不依赖未入库的 `_original/`。

先检查可用磁盘。2026-11-21 本地 Rust 全目标复验在链接阶段遭遇 `No space left on device`，详见 [本轮记录](STATUS.md#2026-11-21-云端交接整理)。资源紧张时可为当前终端设置 `CARGO_INCREMENTAL=0`、`CARGO_PROFILE_DEV_DEBUG=0` 减少开发产物，记录实际配置；不修改仓库工具链、锁文件或降低 CI 门禁。

## 4. 按层验证

### 基础门禁

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo test --locked --doc
node scripts/check-links.mjs
git diff --check
node tests/differential/run.js fast
node tests/differential/run.js fast --whistle 2.10.10
```

当前 fast 有 5 个阶段，包括时钟断言、两套解析语料、二维码和配置迁移；不代表网络和插件行为已验收。新增文档先精确暂存再运行链接检查，因为该脚本只检查 Git 跟踪文件。

### 对当前长连接能力的定向复验

```sh
cargo test --locked --test frame_destroy_e2e
node tests/differential/run.js network --only frames,ws,tcp-control,script-sources,frame-sources,core
node tests/differential/run.js network --only frames,ws,tcp-control,script-sources,frame-sources,core --whistle 2.10.10
```

完整网络与版本矩阵按 [差分 README](../tests/differential/README.md) 执行；两个版本必须使用同一构建制品。2.10.13 的 `ctx.destroy` 等专项按该文档独立安装、固定版本和断言，不能直接替换常规基线。

### 控制台与交付物

浏览器与 Linux 系统依赖的安装见 [控制台 README](../ui-src/README.md#浏览器回归)。三套浏览器测试顺序执行，后两套使用同一代理端口，不能并行启动：

```sh
npm run test:e2e --prefix ui-src
npm run test:e2e:proxy --prefix ui-src
npm run test:e2e:traffic --prefix ui-src
cargo build --locked --release
node scripts/smoke.mjs target/release/whix --console built
```

浏览器、系统依赖与覆盖范围见 [控制台 README](../ui-src/README.md#浏览器回归)。云端不得复用个人浏览器、真实代理数据目录或系统证书信任。

## 5. 后续从哪里开始

先执行 [ROADMAP 的 CLOUD-01](ROADMAP.md#当前待办)：在云端干净检出建立当前可复验基线，再开展后续任务。已实施的 CORE-01…05、QA-01、WS-01/02、TCP-01、SCRIPT-01/02/03 不重新开工。

下一功能切片优先检查本地 `statusCode://101` 模拟连接的帧脚本；握手响应规则及插件能力矩阵分别处理。详细范围、顺序与验收只在 ROADMAP 维护。历史审查中的剩余插件能力须重新核对当前实现，不能把“曾经缺失”直接标成现在仍缺失。

## 6. 提交、推送与接收

每个独立任务结束后，按实际改动路径暂存、检查暂存 diff、运行适用门禁并提交。提交说明简短，不加 AI 署名，不包含本机标识或真实流量。推送按当前用户授权执行；不要强推、改写已有历史或自动发布制品。

`main` push / PR 触发主 CI；完整双版本网络差分是独立的手动/每周任务。CI 构建产物保留 14 天，流程不会自动创建 GitHub Release，也不会部署代理服务。

接收方必须核对远端实际提交与 CI 结果；无法运行的门禁写「受阻」或「未验证」。Git 推送成功、测试通过、正式发布是三个不同结果。

本轮的交付阻塞见 [STATUS](STATUS.md#2026-11-21-云端交接整理)。网络恢复后，在含完整本地提交的仓库执行：

```sh
git fetch --no-tags origin
git log --left-right --oneline HEAD...origin/main
git push origin main
git rev-parse HEAD
git ls-remote origin refs/heads/main
```

先确认远端没有需要处理的新提交，再执行 push；不得强推。最后两条输出的提交号应一致，之后云端再检出。修复网络不需要改变仓库锁文件或关闭 TLS 校验；先前一次连接成功不能视为持续可用。
