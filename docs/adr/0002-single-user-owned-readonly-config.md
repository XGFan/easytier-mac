# 单一用户所有的只读配置文件

GUI 从多 profile 管理器（`profiles/<uuid>.toml`，保存时归一化回写）改为只认一个固定路径的配置文件：`~/Library/Application Support/EasyTier/config.toml`。文件归用户所有，用户用自己的编辑器修改；GUI 只做校验、连接、断开，仅在文件缺失时写入一次带注释的模板，此后永不改写。

理由：归一化回写（`loader.dump()`）会吃掉用户的注释和排版，与「手工编辑」的所有权模型根本冲突——两个写入方必然打架，砍掉 GUI 的写权限是唯一稳定解。固定路径让「哪个文件生效」零歧义。连带简化：不再需要「profile id == instance_id」契约（instance id 由 GUI 在内存跟踪），`state.json` 的 running 集合退化为单个布尔。

否决的替代方案：GUI 保留「校验并修复」式主动回写（重新引入吃注释问题）；保留 profiles/ 目录只认一个文件（uuid 文件名对手工编辑不友好）。
