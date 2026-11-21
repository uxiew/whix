# whix

Rust 调试代理：抓取 HTTP/HTTPS、改写请求与响应、Mock、重放，以及 WebSocket/TCP 调试。一个二进制，内嵌网页控制台，运行核心不需要 Node.js。

A Rust debugging proxy with Whistle-style rules and an embedded web console.

当前版本 `0.1.0`。项目于 2026-10-07 从 `whistle-rs` 更名为 `whix`；已有功能与兼容边界见 [STATUS](docs/STATUS.md)。**云端模型接手从 [AGENTS.md](AGENTS.md) 和 [HANDOFF.md](docs/HANDOFF.md) 开始。**

## 五分钟上手

安装包和校验方法见 [INSTALL](docs/INSTALL.md)；也可按下文从源码构建。

```sh
whix
```

保持代理运行，另开终端执行：

```sh
curl --noproxy '' -x http://127.0.0.1:8899 http://example.com/
```

打开 [控制台](http://127.0.0.1:8899/)，把浏览器的 HTTP/HTTPS 代理设为 `127.0.0.1:8899`。默认只允许本机连接；Ctrl+C 停止。

查看 HTTPS 内容需信任本次实例的根证书，先用 curl 验证：

```sh
curl --noproxy '' -x http://127.0.0.1:8899 --cacert ~/.whix/certs/root.crt https://example.com/
```

各平台安装与撤销证书见 [CERTIFICATES](docs/CERTIFICATES.md)。只转发 HTTPS 时使用 `whix --no-intercept-https`，无需安装根证书。

## 常用规则

在控制台 Rules 中保存后生效：

| 目的 | 规则 |
| --- | --- |
| 线上域名指向本地开发服务 | `www.example.com http://127.0.0.1:5173` |
| 模拟接口失败 | `api.example.com statusCode://503` |
| 增加两秒响应延迟 | `api.example.com resDelay://2000` |
| 修改请求头 | `api.example.com reqHeaders://x-debug=1` |
| 合并 JSON 响应字段 | `api.example.com/me resMerge://{"role":"admin"}` |

无效果时查看请求详情的 Rules，或运行 `whix explain https://example.com/ --rule 'example.com resHeaders://x-debug=1'`。完整场景见 [COOKBOOK](docs/COOKBOOK.md)，语法见 [RULES](docs/RULES.md)。

## 从源码构建

需要 rustup、平台编译工具和 Node.js（`^20.19.0 || >=22.12.0`）。Rust 由 `rust-toolchain.toml` 固定为 1.98.1。

```sh
npm ci --prefix ui-src
npm run build --prefix ui-src
cargo build --locked --release
./target/release/whix --version
```

必须先构建 UI 再编译 Rust；缺少 UI 产物时仅嵌入占位页。云端环境、验证命令及浏览器回归见 [HANDOFF](docs/HANDOFF.md) 与 [DEVELOPMENT](docs/DEVELOPMENT.md)。

## 和 Whistle 的关系

兼容 Whistle 风格规则，常规差分基线固定为 **2.10.8 / 2.10.10**；2.10.13 目前只有定向专项证据。测试范围不能扩张为全量兼容。

插件采用自有 Rust/HTTP/JS SDK 协议，不直接加载上游 `whistle.*` npm 包；HTTP API、控制台与 CLI 也是独立契约。没有 `w2 start/stop` 后台管理层，Weinre 使用外部服务。详见 [PLUGINS](docs/PLUGINS.md)、[API](docs/API.md)、[CLI](docs/CLI.md)。

## 安全须知

默认仅监听回环地址；给局域网设备使用前阅读 [OPERATIONS](docs/OPERATIONS.md)。控制台口令不等于代理访问控制，不要直接暴露公网。请求记录可能含 Cookie/Authorization；根证书私钥、真实抓包与本机配置不能提交到仓库。仅调试有权访问的流量，用完撤销证书信任。

## 文档

[使用手册](docs/COOKBOOK.md) · [规则](docs/RULES.md) · [插件](docs/PLUGINS.md) · [API](docs/API.md) · [开发](docs/DEVELOPMENT.md) · [架构](docs/ARCHITECTURE.md)

[云端交接](docs/HANDOFF.md) · [当前状态](docs/STATUS.md) · [后续计划](docs/ROADMAP.md) · [全部文档](docs/README.md)

推送 `main` 或提交 PR 会触发主 CI；完整双版本网络差分另有手动/每周任务。构建产物上传不等于创建正式 Release。

## 许可

[MIT](LICENSE)。核心行为对照 [avwo/whistle](https://github.com/avwo/whistle)，来源与第三方分发说明见 [NOTICE.md](NOTICE.md)。
