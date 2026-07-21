# ADR-0002：使用 SQLite 作为本地资料库

## Status

Accepted（2026-07-21）

## Context

应用以 Apple Silicon Mac 上的单用户、本地优先工作流为首发形态。任务、字幕、章节和笔记需要在应用重启后保留，但首版不建设服务器或用户账户。

## Decision

使用 SQLite 保存本地资料库。首个表保存分析任务及其状态、来源、时长、创建时间和错误信息；后续以迁移方式增加字幕、章节、笔记和设置表。

## Consequences

### Positive

- 无服务器和持续运营成本。
- 任务可恢复，数据与用户内容留在本机。
- 关系模型适合任务、字幕、章节和笔记的关联查询。

### Negative

- 首版不提供跨设备同步。
- 需要设计数据库迁移与损坏恢复策略。

## Alternatives Considered

**JSON 文件**：未采用，随着任务、全文字幕和章节关系增加，查询、迁移和一致性维护会变差。

**云端数据库**：未采用，会过早引入账号、隐私、网络依赖和运营成本。

## References

- [BRD](../../knowledge/02-requirements/REQ_2026-07-21_quickvideo_app/BRD.md)
