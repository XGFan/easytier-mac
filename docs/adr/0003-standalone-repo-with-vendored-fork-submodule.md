# 独立仓库,以 vendor submodule 依赖 EasyTier fork

easytier-mac 从 XGFan/EasyTier fork 的子目录拆分为独立仓库(`git subtree split` 保留全部历史);对 easytier 的依赖实现为 git submodule `vendor/EasyTier`(钉在 fork `releases/v2.6.4` 分支的具体 commit)+ bridge 的 path 依赖,easytier-core 二进制也从同一 submodule 构建(`scripts/build-core.sh`)。

理由:bridge 链接的 easytier 库(RPC 客户端/配置解析)与 supervisor spawn 的 easytier-core 必须同 commit 构建,否则自研 RPC 协议可能不匹配——一个 submodule pin 同时钉死两个用途,这是决定性论据。其次 core 必须带 fork 的 macOS 全隧道修复(underlay 绑定、拆分默认路由、STUN 绑定),上游与 crates.io 均不可用。fork 分支以 rebase + force-push 维护,任何形式的 rev pin(cargo git 依赖或 submodule)都面临旧 commit 被远端 GC 的风险,对策统一为 fork 侧给 pin 打 tag;submodule 额外换来 vendor/ 内直接改代码联调的能力。

否决的替代方案:cargo git 依赖(rev 随 force-push 漂移,且 core 二进制仍需单独 checkout fork 源码——变成两套机制钉同一个源);crates.io 版 easytier(无 fork 修复,版本与协议都不受控);把 easytier 源码整体复制进本仓库(失去与 fork 的 git 同步/追溯能力,体积失控)。
