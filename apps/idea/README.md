# cyClaw IntelliJ IDEA 插件

这是 cyClaw 的 JetBrains IDE 集成层。插件不复制 Rust Core 逻辑，只通过 `cyclaw mcp` 和 CLI 调用现有项目知识能力。

## 当前能力

- 提供 `cyClaw` Tool Window。
- 展示项目状态、待处理候选知识、待应用文档草稿。
- 通过 `cyclaw mcp` 读取 `get_project_status`、`list_pending_knowledge`、`list_document_patches`。
- 通过 CLI 执行 `init`、`scan`、`observer run --once`。
- 支持启动和停止 `cyclaw observer run` 常驻监听。
- 支持接受、忽略候选知识。
- 支持应用文档草稿，应用前会弹出确认框。

## 开发运行

当前工程目标 IntelliJ IDEA Community 2023.3.8，Java 17。

```powershell
cd apps/idea
.\gradlew.bat buildPlugin
.\gradlew.bat runIde
```

说明：

- 如果当前打开的是 cyClaw 源码仓库，插件会使用 `cargo run -p cyclaw-cli -- ...`。
- 如果当前打开的是其他项目，插件会调用 PATH 中的 `cyclaw` 二进制。
- `gradle-wrapper.jar` 需要由 Gradle wrapper 生成；如果仓库中缺失该文件，可先在有 Gradle 的环境中执行 `gradle wrapper --gradle-version 8.7 --distribution-type bin`。
