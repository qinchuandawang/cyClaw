# 隐私说明

## 本地数据

cyClaw 默认把项目运行数据保存于 `<project>/.cyclaw/`，包括项目画像、知识候选、文档 Patch、任务、结构化事实、索引、事件和 Agent 运行记录。这些数据可能包含项目中的路径、摘要和文档片段，因此 `.cyclaw/` 默认被 Git 忽略。

## 模型 Provider

cyClaw 支持用户配置 OpenAI-compatible 模型 Provider。只有在用户配置 Provider、授予模型/联网权限并触发模型审查或 Agent 运行时，相关上下文才会发送到该 Provider。发送范围取决于当前任务、候选和上下文预算，而不是整个仓库。

请在启用外部模型前确认 Provider 的数据处理政策、所在区域和组织合规要求。不要把不允许离开本机的内容发送到外部服务。

## API Key 与遥测

- CLI 配置只保存 API Key 的环境变量名，不保存 Key 本身。
- VS Code 扩展使用 VS Code SecretStorage 保存用户输入的 API Key。
- cyClaw 当前不包含产品遥测、行为分析或云端项目同步。

## 删除数据

停止使用某个项目时，删除该项目的 `.cyclaw/` 目录即可删除 cyClaw 在该项目保存的本地状态。删除前请确认不再需要审计记录、可撤销 Patch 和任务记忆。
