# 快看视频 QuickVideo（macOS / Windows MVP）

把一段 YouTube / Bilibili 视频的可用字幕，或用户选择的本地音视频，整理成可导出的 Markdown 视频笔记。

## 当前可用流程

1. 粘贴 YouTube 或 Bilibili 链接：先尝试取得平台可用字幕；若没有字幕，则只临时提取音频交给本机 `whisper.cpp` 转写。应用不保存完整视频，也不处理付费、登录、DRM 或其他受限内容。
2. 或选择本地视频/音频：使用本机 `whisper.cpp` 转写，临时 WAV / SRT 在处理完成后清理。
3. 在“模型”页填入自己的 SiliconFlow API Key 与模型名；Key 以明文保存在本机应用配置中，字幕才会被发送到 SiliconFlow 的固定 API 端点以生成摘要。
4. 打开完成的笔记，在原生保存面板中导出 Markdown（摘要与完整字幕）。

支持中英文及混合内容；默认本地模型为 `ggml-base`，长视频建议接通电源并预留处理时间。

## 试用版安装与运行

macOS 调试构建产物：

- `src-tauri/target/debug/bundle/macos/QuickVideo.app`
- `src-tauri/target/debug/bundle/dmg/QuickVideo_0.1.0_aarch64.dmg`

本机运行时：

- macOS：通过 Homebrew 或其他方式安装 `ffmpeg`、`whisper-cli` 与 `yt-dlp`（仅链接字幕）；应用会额外检查 `/opt/homebrew/bin` 与 `/usr/local/bin`。
- Windows：安装 `ffmpeg.exe`、`whisper-cli.exe` 与 `yt-dlp.exe` 并加入系统 `PATH`；也可将它们放入 `QuickVideo.exe` 同级的 `tools` 文件夹。可用 `QUICKVIDEO_TOOLS_DIR` 指向自定义工具目录。
- 本地模型保存于各系统的 QuickVideo 应用数据目录；缺少时可在应用“设置”页下载。

应用会在启动时检查这些组件。首次启动新版时，会自动迁移旧版 Video Knowledge 的资料库和本地转写模型；缺少组件时，本地导入或链接字幕会失败，但已有笔记仍可浏览、导出。

缺少 `ggml-base` 时，可在应用的“设置”页直接下载；模型来自 [whisper.cpp 官方模型仓库](https://huggingface.co/ggerganov/whisper.cpp)，下载后保存于应用数据目录。

开发命令：

```bash
cd app
npm run tauri dev
```

验证与打包：

```bash
npm run build
cargo check --manifest-path src-tauri/Cargo.toml
npm run tauri build -- --debug
```

## 自动构建与发布

GitHub Actions 工作流位于 [`.github/workflows/build-desktop.yml`](../.github/workflows/build-desktop.yml)。普通分支提交或手动运行仅构建并保存 7 天的测试 Artifact；推送形如 `v1.0.0` 的标签时，工作流会构建 macOS 与 Windows 安装包，并自动创建 GitHub Release。

macOS 正式分发仍需要在 GitHub Secrets 中配置 Apple 签名与公证凭据；Windows 正式分发建议后续配置代码签名证书，以避免系统的未签名应用警告。

## 当前范围与发布前缺口

这是个人测试 MVP，不是可直接销售的正式发行版：外部工具和模型尚未内置到 App 包中；DMG 也尚未使用 Apple Developer ID 签名或公证。公开销售前必须解决可重复安装、许可证核查、签名/公证、隐私说明，以及平台授权与内容使用边界。

产品决策及发布策略见 [`../docs/plans/2026-07-21-distribution-plan.md`](../docs/plans/2026-07-21-distribution-plan.md)。
MVP 的[隐私说明](../docs/legal/PRIVACY.md)与[内容使用规则](../docs/legal/CONTENT_USE.md)也已随项目保存。
公开销售前的组件许可与打包复核清单见 [THIRD_PARTY.md](../docs/legal/THIRD_PARTY.md)。
