# 快看视频 QuickVideo

QuickVideo 是一款运行在 Apple Silicon Mac 上的本地视频知识整理工具：导入本地音视频，或在授权与平台规则允许的范围内分析 YouTube / Bilibili 链接，将字幕整理为带时间线章节的 Markdown 笔记。

## 项目结构

- [`app/`](app/)：Tauri 桌面应用及其构建说明。
- [`docs/`](docs/)：产品决策、分发规划、隐私说明与内容使用规则。
- [`knowledge/`](knowledge/)：需求背景、BRD 与已确认的决策记录。

## 当前状态

这是个人测试 MVP。它使用本机 `whisper.cpp` 转写；摘要通过用户自行配置的 SiliconFlow API Key 生成。公开销售前仍需完成签名、公证、第三方许可证复核和安装升级测试。

详细的安装、开发和打包步骤见 [`app/README.md`](app/README.md)。
