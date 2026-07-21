# ADR-0001：MVP 使用用户自带 API Key 生成摘要

## Status

Accepted（2026-07-21）

## Context

本产品首版为 Apple Silicon Mac 上运行的个人视频知识整理工具。字幕与转写可在本地完成，但章节和摘要需要语言模型能力。产品尚处于价值验证阶段，不希望先建设用户账户、计费、模型代理或云端运营体系。

## Decision

首版采用 BYOK（Bring Your Own Key）模式：用户自行配置 API Key 与模型服务，应用使用该配置生成章节和摘要。首版不提供自带 Key、免费额度或托管模型服务。

## Consequences

### Positive

- 无需承担首版模型调用成本。
- 可让用户按质量、价格、地域和偏好选择模型服务。
- 避免在产品验证期建设账户、支付和云端计量系统。

### Negative

- 首次使用需要用户完成 Key 配置。
- 不同模型服务的结果质量、上下文长度和错误形式存在差异。
- 需要安全存放密钥，且需要清晰提示调用费用由用户承担。

### Neutral

- 后续增加官方托管 Key 或额度时，仍可保留 BYOK 作为高级用户选项。

## Alternatives Considered

**完全本地摘要模型**

- 未在首版采用：下载体积、机器性能差异和结果质量会增加首次体验与支持成本。

**官方统一云端摘要服务**

- 未在首版采用：需要先处理账户、计费、服务稳定性、隐私说明和持续成本。

## References

- [需求决策记录](../../knowledge/02-requirements/REQ_2026-07-21_quickvideo_app/DECISIONS.md)
