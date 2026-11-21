# whistle-rs 核心覆盖复审

> 历史归档（2026-11-21 纳入仓库）：以下正文记录 2026-09-30 的 `bd378ec8`，保留当时名称、结论与统计。CORE-01…05、QA-01 后来已实施，EXT/CTRL 也已有选择性落地；不能直接据本文重开任务。当前入口见 [STATUS](../STATUS.md)、[ROADMAP](../ROADMAP.md) 和 [HANDOFF](../HANDOFF.md)。正文中 `target/` 证据不随 Git 交付。

日期：2026-09-30

代码：`bd378ec8b1120f356cd70dd115434eb3b7f9a488`，分支 `main`。

本轮调试二进制 SHA-256：`5f0b470b5c5d3855287ce68b8ef8e2596c62762b80b1c148c295476f45fd6efd`。

本轮检查未修改项目源码、文档、依赖或系统设置；新增探针结果只存于工作区忽略的 `target/`。本文是独立审查报告，不是把已完成路线图重新打开。

## 1. 结论

已有路线图完成，不等于上游核心能力全部覆盖。现有工程、失败会话、查询、诊断、源站 h2、连接复用、发布基础设施等工作不应再作为“尚未实现”重复安排。但本轮新增加的受控探针仍复现了认证插件发现阶段、TLS 客户端证书、JavaScript 正则与辅助函数、帧脚本、TCP 数据处理、页面 Console 日志等差异。

需要区分三类问题：

| 类别 | 定义 | 本轮代表 |
|---|---|---|
| 产品缺陷 | 即便不追求上游兼容也应修复 | manifest 失败导致认证能力被缓存为无认证 |
| 核心语义缺口 | 已采用上游同名规则，但行为仅部分实现 | tlsOptions 客户端证书；frameScript 状态、二进制、TCP；log 页面日志 |
| 有意不兼容 | 产品已经选择独立契约，但仍须准确描述影响 | 原生 npm 插件加载、Node 对象 API、whistleBridge、原版 CGI/API |

尤其不能用“插件协议不同”解释所有差距。mTLS、普通 JavaScript 正则、frameScript 和 log 的反例不依赖运行原版 npm 插件。

## 2. 本轮证据与限制

环境：macOS ARM64；Node.js v24.9.0；`cargo +stable --version --verbose` 为 Cargo 1.98.0。固定 `rust-toolchain.toml` 的 1.98.1 下载遇到 `static.rust-lang.org` TLS handshake EOF，未修改该文件，改用已安装 stable 做补充检查。这不等同于固定 1.98.1 验收。

| 检查 | 结果 | 限定 |
|---|---|---|
| `cargo test --locked --all-targets --quiet` | 环境阻塞 | 固定工具链同步失败，不能解释为产品测试失败 |
| `cargo +stable test --locked --all-targets --quiet` | 1063 单元 + 38 集成通过；8 ignored | 忽略项没有执行；不是此次重新完成全部跨平台门禁 |
| `cargo +stable build --locked --quiet` | 通过 | 新构建的当前提交制品 |
| `node run.js fast` | 2.10.8：3/3 步通过 | 文档解析、手写解析、二维码 |
| `node run.js fast --whistle 2.10.10` | 2.10.10：3/3 步通过 | 同一 Rust 制品 |
| 新增 HTTP/脚本/认证探针 | 11 个场景执行完毕，发现下列差异 | 探针退出 0 表示采集完成，不表示全部兼容 |
| 新增 TLS/WS/TCP/认证对照 | 7 个场景执行完毕，发现下列差异 | 一次性测试 CA，不安装系统信任；源站、插件均为回环夹具 |

两个快速基线均记录：文档解析 17462 个问题、4730 命中，未知算子与值差异均 0；手写解析 2023 个问题、1735 命中，未知差异 0，同时存在 declared=7、declared values=1。不能省略这些例外，把结果描述成未经归一化的全量相同。

本轮没有重跑全套网络差分、原版 280 调用套件、浏览器端到端、压力/长时间稳定性或各平台发布矩阵。那些现有验收记录只作历史证据查阅。

## 3. 实测确认的缺陷和核心差距

### A01 — 插件 manifest 获取失败可使认证失效（最高优先级）

位置：`src/plugins/mod.rs`，`RemotePlugin::manifest` / `on_request`，约 588–650 行。

当前路径：manifest 使用 `OnceCell`；请求失败、非 200 或解析失败均生成并缓存 `PluginManifest::v1_fallback`；只有 manifest 声明 auth 才会调用认证钩子。于是一次暂时性错误被转换为持久的“这个插件没有认证能力”。

本轮本地场景：注册 `--plugin gate=127.0.0.1:<port>`，规则命中 `plugin://gate`。首次 `/manifest` 返回 503；随后插件恢复正确 manifest。

| 观测 | 结果 |
|---|---|
| 第一次代理请求 | 200，到达源站 |
| manifest 恢复后的第二次请求 | 仍为 200 |
| manifest 调用次数 | 1 |
| auth 调用次数 | 0 |
| 新代理实例一开始就取得有效 auth manifest，再让 `/auth` 返回非成功状态 | 502，认证失败被正确阻断 |

准确的缺陷条件是“首次发现/解析失败”，不是已取得有效 manifest 后的每次认证故障都会放行。认证钩子本身的失败关闭逻辑存在，但其前置能力发现阶段没有相同保证。

建议：显式声明协议版本和必要能力；将暂时不可用、缺少旧协议端点、格式错误分开；不要永久缓存失败为 v1；安全关键插件在能力未确认时不得放行。增加启动竞争、恢复、错误 manifest、必要能力缺失等测试，且应断言源站是否被访问，不只看状态码。

### A02 — tlsOptions 不是完整 TLS 配置：客户端证书未接入

官方说明包含客户端证书与 mTLS、TLS 版本及多条配置合并。[S1]

本项目已实现版本和套件配置、表达式求值及多条合并，不应重复安排这些内容。实际缺的是 TLS 身份等配置在连接层的消费：`src/proxy/apply/route.rs` 将配置合并后主要读取版本/套件；`src/proxy/upstream.rs` 的 TLS ClientConfig 路径使用 `with_no_client_auth()`（约 592–612 行）。

本轮建立要求客户端证书的临时 HTTPS 源站，以普通 HTTP 请求经过代理后转发到该 TLS 源站，排除客户端信任代理 CA 的干扰。源站校验的是一次性测试 CA 签发的客户端证书。

| 配置 | Whistle 2.10.10 | whistle-rs |
|---|---|---|
| 不提供客户端证书，负对照 | 502 | 502 / CertificateRequired |
| `tlsOptions://key=<client.key>&cert=<client.crt>` | 200，源站确认 `authorized=true` | 502 / CertificateRequired |

因此，SNI 证书钩子、代理根 CA、自备 MITM 证书并不能代替“代理向源站提交客户端身份证书”。PFX/passphrase、每规则 CA/信任和其余 TLS 对象字段也需要独立的支持表与验证，不能因对象能解析便宣称已经实现；本轮没有逐一实测这些附加字段。

建议优先补 PEM key/cert，然后明确 PFX 及信任参数策略。连接池键必须隔离客户端身份、信任根、SNI、目标与代理路由，不能只把证书塞进全局默认连接配置。TLS 1.0/1.1、RC4/3DES 等后端不提供的算法另列明确不支持，不应为了复制旧能力默认降低安全性。

### A03 — 规则正则不完整等价于 JavaScript 正则

官网将规则正则描述为 JavaScript 兼容。[S2] 本项目主规则/过滤器和替换路径使用的 regex 语义并不覆盖所有 JS 构造。

本轮规则：

```text
/probe(?=\.test)/ reqHeaders://x-probe=hit
probe.test reqHeaders://x-probe=hit includeFilter://m:/^GET(?=$)/
```

分别访问受控 `probe.test` / GET：两例上游均向源站注入 `x-probe=hit`，Rust 端均没有注入。另做不匹配过滤器的负对照，两者均未注入；本轮没有据此声称存在过滤器失败开放。

定位：`src/rules/mod.rs` 的正则/过滤器解析，约 3969 行的测试也说明无法编译的表达式会作为字面值处理；`src/proxy/restream.rs` 等有独立正则消费点。

建议按 pattern、filter、req/resReplace、模板替换分别建立子集表和测试。优先解决合法 JS 表达式静默失配的问题；确实不支持的构造应给出明确位置及诊断。扩展兼容后还须限定执行成本，不把复杂表达式引入为无界计算。

### A04 — reqScript/resScript 辅助环境存在真实差异

官方将 `parseQuery`、`parseUrl` 分别对应到 Node.js querystring.parse 和 url.parse。[S3] 当前 `src/proxy/script.rs:367–393` 是简化实现。

| 输入/观察 | Whistle 2.10.10 | whistle-rs |
|---|---|---|
| `parseQuery('a=1&a=2&q=a+b')` | `a` 为数组 `['1','2']`，`q='a b'` | `a='2'`，`q='a+b'` |
| parseUrl 带用户信息、IPv6 和 hash | auth/hostname/hash 分离 | 用户信息混入 host/hostname，hash 为空 |
| `Buffer.from('A').toString('hex')` 用于生成规则 | 生成值 `41` | 未生成对应规则 |
| 规则脚本读取 pattern、port | 实际匹配模式与监听端口 | pattern 空字符串、port 为 0 |
| `render('<%= n %>', {n:7})` 对照 | 7 | 7，已实现，不是缺口 |

源码还明确未提供 `decodeBuffer`、`encodeString`、`encodingExists`；这几项本轮以代码审查为依据，没有逐项运行所有编码组合。`httpVersion` 的脚本值固定为 1.1，H2 情形需专项补测；不能用本轮 HTTP/1 请求的相同结果证明该字段完全准确。

建议修复辅助函数语义、准确请求上下文以及字节/编码桥接，同时让脚本错误进入结构化诊断。不要把 HTTP 插件的 30 秒超时当成同步 JS 执行预算；脚本 CPU/内存/中断也要形成明确契约。

### A05 — frameScript 部分实现：状态、二进制、TCP

官方将 frameScript 用于 WebSocket 和普通 TCP 数据帧。[S4]

本轮连续状态脚本：

```js
var n = 0;
ctx.handleSendToServerFrame = function(buf) {
  return 'N' + (++n) + ':' + buf;
};
```

| 场景 | Whistle 2.10.10 | whistle-rs |
|---|---|---|
| 同连接依次发送 a/b/c | N1:a、N2:b、N3:c | N1:a、N1:b、N1:c |
| 二进制 BINARY，handler 加 X | XBINARY | BINARY |
| 普通 TCP + inspect，handler 加 X | XRAW_AUDIT | RAW_AUDIT |

定位：`src/proxy/script.rs:527` 的每帧 `Context::default()`；`src/proxy/ws.rs:746` 只对文本帧调用该函数；TCP 透明转发路径未接同样的帧脚本执行。

另外，handler 内 sendToClient/sendToServer 的动态注入、options、分片/压缩语义也需要独立验证，不能由启动时预设帧注入推导为全部支持。本轮 frameScript 中 `typeof url` 的额外对照两边均为 undefined；虽然官网列了相关全局变量，未把这个结果算成 Rust 单方缺陷。

建议提供按连接生存的脚本上下文，支持明确的二进制和 TCP 字节处理契约；覆盖 FIN、分片、压缩、取消、背压与资源限制，避免简单逐帧新建上下文。

### A06 — log 规则未实现原版页面 Console 功能，Weinre 也未闭环

官网 log:// 是页面脚本注入、console/异常采集与管理界面日志展示，不是代理访问日志或会话标签。[S5]

本轮使用 31 字节 HTML 源站并设置 `log://audit`：上游返回 12053 字节且包含 console 注入内容；Rust 返回原始 31 字节，没有页面日志注入。应承认它是同名规则效果缺失，而非仅控制台样式不同。

Weinre 本项目有 HTML 脚本标签注入（`src/proxy/response.rs:516–522`），但普通名称会形成代理自身 `/weinre/target/target-script-min.js` URL（约 941–956 行），当前控制台路由没有提供完整 Weinre 服务。外部服务地址是可选组合方案，不等于内置目标脚本、通信和 Inspector 完整实现；本轮未执行真实浏览器 DOM 调试。

建议把“页面 Console 日志”与“代理日志”分开命名。Weinre 可选择显式可选组件，或清楚声明外置依赖；不应默认构造一个没有对应内置服务的地址并声称完整支持。

## 4. 插件 API / 规则配置覆盖矩阵

现有 RustPlugin、HTTP/Node SDK 确有请求/响应、auth、SNI、流式、WS、stats、UI 钩子。下面列的是与官方插件体系的差别，不是宣称本项目没有插件。

| 官方能力 | 当前边界 | 证据/判断 |
|---|---|---|
| 插件 rules.txt / _rules.txt / resRules.txt / _values.txt 与包内变量 | 没有官方包加载与自动注册的整套生命周期 | 本项目插件从 Rust 注册或 CLI 远端/Node 入口加入；需独立适配器或明确不支持 [S6] |
| @ 插件来源 | `@whistle.<name>/...` 和 `@$...` 分支不解析为实际插件内容 | `src/rules/include.rs:459–468`；普通文件/URL include 已有，不能说整个 @ 不支持 [S7] |
| % 全局、按请求插件变量 | 没有对应 globalPluginVars/pluginVars 上下文 | `PluginReq`/manifest 没有此模型；不可用普通 Values 替代作用域语义 [S8] |
| rulesServer 的动态 rules + values | 有请求返回 rules 文本；不是完整返回对象兼容 | 可经内嵌 Values 文本实现部分效果，但没有同构的独立 values 返回契约 [S6] |
| resRulesServer | on_response 可改 status/headers/body，但没有动态返回规则入口 | `PluginResResult`，`src/plugins/mod.rs:447`；改结果字段不等于重新参与响应期规则解析 [S6] |
| tunnelRulesServer、TCP pipe | 缺少相应隧道阶段/字节处理钩子 | HTTP pipe、WS hook 不能自动用于原始 TCP；frameScript TCP 已实证缺口 [S6][S9] |
| reqRead/reqWrite、resRead/resWrite 分阶段管线 | 当前主要是 pipeRequest/pipeResponse 和 WS hook | 两个方向不等于原版“抓包前读/发出前写”的多个阶段；明文显示但密文上线需明确模型 [S6][S9] |
| getReqSession/getSession/getFrames、storage/sharedStorage/sessionStorage、compose/请求助手 | 没有官方同构便利 API | 自有 REST 和用户自己保存状态只能替代部分用途，需梳理能力矩阵与权限 [S10] |
| whistleBridge、扩展详情页/Composer/菜单、规则补全 | 插件可提供独立页面，但不能据此认为扩展了宿主面板 | 独立 /plugin 页面与宿主扩展契约不同 [S6] |
| 插件 UI 的 WebSocket upgrade | 当前文档明确不支持 | `docs/PLUGINS.md:864`；HTTP 页面能打开不证明它的 WS 通道可工作 |
| 官方短协议 `<name>://` | 当前有 `plugin://name`、`whistle.name://` 等，不是任意已注册短名解析 | `src/rules/mod.rs:2931–3052`；本项为源码核验，未做原生 npm 安装后的黑盒比较 |

决定继续采用自有插件系统是合理的既定方向。这里需要的是“能力对等/可替代/不支持”分类，不是被迫完整复制 Node req/res 对象。以 provider/适配层补变量、资源、动态响应规则、存储及 UI 扩展等能力，也不要求代理核心绑定 Node。

## 5. 控制接口与全局配置：不全是 URL 不同

本项目 `src/proxy/webui.rs:83–130` 的路由已经支持较多规则、Values、会话、搜索、Composer、帧操作，但未提供以下对等入口：

- 全局 HTTPS 拦截的运行时读写开关；启动参数或改写 capture 规则不是相同状态模型。
- 原版临时规则文件 createFile/getFile 及 temp/... 编辑工作流。
- 全局规则开关、后置规则优先、多选状态等统一控制模型；逐组 toggle 不能自动复原原版状态语义。
- 插件的运行时启停/重载、官方包生态控制以及插件扩展 API。

官方 Local Agent API 对 HTTPS 开关、临时文件、规则全局状态等有明确接口说明。[S11] 因此不能再笼统说“能力都有，只是路径/JSON 不一样”。本项目可以保留自有路径，但仍应决定是否补能力。

配置与传输的其它已知范围：源站 TLS h2 已实现；h2c/httpH2 未实现；`disable://http2` 的客户端侧行为仍有具名差异（`tests/differential/h2-bench.js`）。`-r` 仍是 Default 组而非不可见 shadowRules 层；自定义 DNS server/cache 配置未提供。它们应列在支持范围，不要求没有具体需求就实现所有部署形态。[S12]

## 6. 为什么现有门禁仍会全部通过

这不是旧计划没有工作量，而是测试命题有限。

`tests/differential/README.md:194–214` 记录：原版套件适配当前 judged 180，passed 160，declared 20；其余调用没有作为同等判据。尤其其中 6 条通过只因为原始断言检查响应是否 JSON，而本项目缺失的 /cgi-bin 端点返回 JSON 404。该数字不是 160 项能力已实现的证明。本輪未重新执行该套件，引用的是其明确披露的统计与限制。

解析 oracle 不覆盖真实 mTLS、持续帧状态、页面脚本执行、manifest 首次故障、完整插件包生命周期。新探针找到的差异与既有快速语料全绿可以同时成立。

建议新增真实后置条件：改规则后必须观察匹配/转发结果，插件副作用要检查对应阶段实际调用，错误状态必须验证源站未访问；JSON 可解析、HTTP 有返回、规则名出现在语料里，都不足以验收功能。

## 7. 下一阶段建议（新任务，不回滚旧里程碑）

| 编号 | 优先级 | 工作 | 关键验收 |
|---|---|---|---|
| CORE-01 | P0 | manifest 与必要安全能力发现 | 首次 503/连接拒绝/畸形 manifest/恢复均有测试；必要 auth 未确认时源站不可达；明确 legacy opt-in |
| CORE-02 | P1 | TLS 身份与支持字段 | 无证书负对照失败、正确证书成功、错误/过期/不同租户证书拒绝；连接池身份隔离；未支持字段显式诊断 |
| CORE-03 | P1 | JS 正则与脚本语义 | 本文 parseQuery/parseUrl/Buffer/元数据/正则反例成为回归；能力范围、CPU/内存/中断边界明确 |
| CORE-04 | P1 | 有状态帧与 TCP 扩展 | N1/N2/N3 状态保持；二进制/TCP 变换；注入、分片、压缩、背压与取消测试 |
| CORE-05 | P1/P2，取决于手机/浏览器调试需求 | 页面 Console 与 Weinre 路径 | 必须用浏览器实际产生日志/异常并在宿主查看，不能只检查标签注入；外置依赖可发现/可诊断 |
| EXT-01 | P1/P2，按需要选 | 自有插件的能力扩展/独立上游适配层 | 资源规则、变量、动态 resRules、流阶段、存储/会话、UI bridge 分开验收；不许只改名称声称对等 |
| CTRL-01 | P2 | 全局控制、临时规则文件、插件生命周期 API | 接口效果、授权、幂等、状态重启行为；保留自有 API，不要求盲目复制原版 CGI |
| QA-01 | 与以上同步 | 上游示例的有判别力测试 | 处理 6 条 JSON 弱通过；正负对照、实际调用阶段、状态持续性、客户端与源站双侧证据 |

跨平台交付、源站 h2、失败会话等已完成项不再重复排为零起点。原版 npm 包兼容是独立产品选择；即使不做，也仍应先处理 CORE-01 及已承诺同名规则的真实效果。

## 8. 证据定位

当前工作区忽略目录中的结果：

- `target/core-audit-results.json`：11 个 HTTP/脚本/manifest 场景。
- `target/protocol-audit-results.json`：7 个 TLS/WS/TCP/认证对照场景。
- `target/differential/2026-09-30T10-06-08-825Z-fast/`：固定 2.10.8 快速差分。
- `target/differential/2026-09-30T10-10-56-008Z-fast-whistle-2.10.10/`：固定 2.10.10 快速差分。

报告列出的新反例均来自同一新构建制品。临时代理/源站/插件均已退出，一次性 CA 文件夹已清理。日志中的测试域名/用户信息都是夹具，不是对真实业务系统的请求。

## 官方依据（2026-09-30 查阅）

- [S1 tlsOptions](https://wproxy.org/docs/rules/cipher.html)
- [S2 pattern / JavaScript 正则](https://wproxy.org/docs/rules/pattern.html)
- [S3 reqScript](https://wproxy.org/docs/rules/reqScript.html)
- [S4 frameScript](https://wproxy.org/docs/rules/frameScript.html)
- [S5 log](https://wproxy.org/docs/rules/log.html)
- [S6 插件开发](https://wproxy.org/docs/extensions/dev.html)
- [S7 @ includes](https://wproxy.org/docs/rules/@.html)
- [S8 % 插件变量](https://wproxy.org/docs/rules/plugin-vars.html)
- [S9 pipe](https://wproxy.org/docs/rules/pipe.html)
- [S10 官方插件类型定义，官网链接的 avwo/lack](https://github.com/avwo/lack/blob/master/assets/ts/src/types/global.d.ts)
- [S11 Local Agent API](https://wproxy.org/docs/extensions/api.html)
- [S12 enable](https://wproxy.org/docs/rules/enable.html)
- [上游代码](https://github.com/avwo/whistle)

官网是动态文档，不等于某个 npm 版本的可执行规范。遇到官网描述与固定版本行为不同，报告应分别注明；本轮没有把两边相同的 frameScript 全局变量结果误记为 Rust 单方回归。
