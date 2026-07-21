# 第三方组件与发布复核清单

更新日期：2026-07-21。此清单服务于当前 MVP 的发布准备，不构成法律意见；在公开销售、内置二进制或更换依赖版本前必须再次复核。

| 组件 | 当前用途 | 当前发布形态 | 初步许可信息 | 公开销售前行动 |
|---|---|---|---|---|
| Tauri / 官方插件 | macOS 壳、原生对话框、打开文件 | Rust/JS 编译进应用 | MIT 或 Apache-2.0 | 生成完整第三方通知与依赖清单。 |
| whisper.cpp | Apple Silicon 本地转写 | 当前通过用户本机安装调用 | MIT | 若内置，附带 MIT 许可证及版权声明。 |
| ggml-base 模型 | 本地中英转写模型 | 用户主动从官方仓库下载 | 仓库模型卡标示为 MIT；当前 SHA-1 已固定校验 | 复核模型仓库与模型卡版本、分发权利和归属说明。 |
| FFmpeg | 音频规范化与时长读取 | 当前通过用户本机安装调用 | 基础为 LGPL-2.1+；启用可选 GPL 组件会改变义务 | 选择并记录可再分发构建；确认配置、动态链接/源码提供和 NOTICE 义务。 |
| yt-dlp | 平台字幕获取与临时音频处理 | 当前通过用户本机安装调用 | 源码为 Unlicense，但发行物可能含其他许可组件 | 不将 Homebrew 版本直接复制进发行包；选择明确的官方发行形式并附带全部第三方许可证。 |
| SQLite / rusqlite | 本地资料库 | Rust 依赖编译进应用 | 待以锁文件生成完整软件物料清单 | 生成 SBOM 并逐项复核传递依赖。 |

## 当前结论

当前 DMG 是个人测试包，**不含**上述外部工具和模型，因此不应被描述为零配置或直接售卖的正式安装包。正式发布必须先完成：

1. 可重复、可审计的 Apple Silicon 运行时打包方案；
2. 许可文本、NOTICE、SBOM 与模型分发复核；
3. Apple Developer ID 签名和 notarization；
4. 首次安装、升级、离线本地文件与授权链接的回归测试。

## 主要参考

- [Tauri license](https://v2.tauri.app/concept/architecture/)
- [whisper.cpp MIT license](https://github.com/ggml-org/whisper.cpp/blob/master/LICENSE)
- [whisper.cpp 官方模型仓库](https://huggingface.co/ggerganov/whisper.cpp)
- [FFmpeg License and Legal Considerations](https://www.ffmpeg.org/legal.html)
- [yt-dlp licensing note](https://github.com/yt-dlp/yt-dlp/blob/master/README.md?plain=1)
