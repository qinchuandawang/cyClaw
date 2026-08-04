import * as cp from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";

type McpToolName =
  | "get_project_status"
  | "list_pending_knowledge"
  | "list_document_patches"
  | "list_fact_patches"
  | "apply_fact_patch"
  | "revert_fact_patch"
  | "get_policy"
  | "get_model_providers"
  | "list_events"
  | "begin_task"
  | "get_active_task"
  | "get_task_context"
  | "record_decision"
  | "reconcile_project_knowledge"
  | "close_task"
  | "get_latest_reconciliation";

interface ProjectStatus {
  initialized: boolean;
  git_has_changes: boolean;
  inbox_pending: number;
  draft_pending: number;
  fact_patch_total: number;
  fact_patch_pending: number;
  fact_patch_revertible: number;
  index_exists: boolean;
  suggested_next_steps: string[];
}

interface KnowledgeCandidate {
  id: string;
  summary: string;
  importance: string;
  recommended_doc: string;
  related_files: string[];
  reasons: string[];
  confidence: number;
  reviewed_by_model: boolean;
  model_recommendation?: string;
  model_rationale?: string;
}

interface DocumentPatch {
  id: string;
  summary: string;
  target_doc: string;
  candidate_id: string;
  operation: "create" | "update" | "merge" | "supersede" | "delete";
  selector?: string;
  source_selectors: string[];
  delete_target_document: boolean;
  original_content: string;
  proposed_content: string;
  preview: string;
}

interface FactPatch {
  id: string;
  operation: "create" | "update" | "merge" | "supersede" | "delete";
  target_fact_id?: string;
  source_fact_ids: string[];
  before: Array<{ id: string; statement: string; status: string }>;
  after: Array<{ id: string; statement: string; status: string }>;
  preview_fingerprint: string;
  status: "pending" | "applying" | "applied" | "reverting" | "reverted";
}

interface AgentEvent {
  id: string;
  event_type: string;
  created_at: string;
  summary: string;
  data: Record<string, unknown>;
}

interface TaskRecord {
  id: string;
  title: string;
  objective: string;
  status: "active" | "closed";
  related_files: string[];
  context_budget_tokens: number;
  decisions: Array<{ id: string; statement: string; rationale: string; evidence: string[] }>;
  failed_approaches: Array<{ id: string; approach: string; reason: string; evidence: string[] }>;
  checkpoints: Array<{ id: string; summary: string; related_files: string[] }>;
}

interface TaskContextPack {
  facts: {
    facts: Array<{
      fact: { id: string; statement: string; fact_type: string; evidence: string[]; confidence: number };
      relevance_score: number;
      relevance_reason: string;
    }>;
    estimated_tokens: number;
  };
  documents: Array<{ path: string; title: string; snippet: string }>;
  estimated_tokens: number;
  budget_tokens: number;
  truncated: boolean;
}

interface ReconciliationReport {
  id: string;
  duplicate_count: number;
  conflict_count: number;
  stale_count: number;
  findings: Array<{
    id: string;
    kind: "duplicate" | "conflict" | "stale";
    reason: string;
    recommended_operation: string;
    confidence: number;
  }>;
}

interface KnowledgeSnapshot {
  status?: ProjectStatus;
  candidates: KnowledgeCandidate[];
  patches: DocumentPatch[];
  factPatches: FactPatch[];
  policy?: PolicySnapshot;
  models?: ModelProvidersSnapshot;
  events: AgentEvent[];
  activeTask?: TaskRecord;
  taskContext?: TaskContextPack;
  reconciliation?: ReconciliationReport;
  error?: string;
}

interface PolicySnapshot {
  permissions: {
    allow_model_call: boolean;
    allow_network: boolean;
    allow_shell: boolean;
    allow_code_write: boolean;
    allow_docs_apply: boolean;
    allow_auto_apply_docs: boolean;
  };
  automation: {
    auto_apply_min_confidence: number;
  };
}

interface ModelProvider {
  base_url: string;
  model: string;
  api_key_env: string;
  thinking_enabled: boolean;
}

interface ModelProvidersSnapshot {
  active_provider?: string;
  providers: Record<string, ModelProvider>;
}

interface ModelSecretBinding {
  workspaceRoot: string;
  providerName: string;
  apiKeyEnv: string;
}

type PermissionAction = "docs" | "model" | "autoDocs" | "shell" | "code";

class CyclawTreeItem extends vscode.TreeItem {
  constructor(
    label: string,
    collapsibleState: vscode.TreeItemCollapsibleState,
    public readonly kind: "section" | "leaf" | "candidate" | "patch" | "factPatch" | "permission" = "leaf",
    public readonly id?: string,
    public readonly children: CyclawTreeItem[] = [],
    public readonly permissionAction?: PermissionAction
  ) {
    super(label, collapsibleState);
  }
}

class CyclawKnowledgeProvider implements vscode.TreeDataProvider<CyclawTreeItem> {
  private readonly changed = new vscode.EventEmitter<CyclawTreeItem | undefined>();
  readonly onDidChangeTreeData = this.changed.event;
  private snapshot: KnowledgeSnapshot = { candidates: [], patches: [], factPatches: [], events: [] };
  private dashboard?: CyclawDashboardProvider;

  attachDashboard(dashboard: CyclawDashboardProvider): void {
    this.dashboard = dashboard;
    dashboard.refresh(this.snapshot);
  }

  getCandidate(id: string): KnowledgeCandidate | undefined {
    return this.snapshot.candidates.find((candidate) => candidate.id === id);
  }

  getCandidates(): KnowledgeCandidate[] {
    return [...this.snapshot.candidates];
  }

  getPatch(id: string): DocumentPatch | undefined {
    return this.snapshot.patches.find((patch) => patch.id === id);
  }

  getFactPatch(id: string): FactPatch | undefined {
    return this.snapshot.factPatches.find((patch) => patch.id === id);
  }

  refresh(snapshot: KnowledgeSnapshot): void {
    this.snapshot = snapshot;
    this.changed.fire(undefined);
    this.dashboard?.refresh(snapshot);
  }

  notify(): void {
    this.changed.fire(undefined);
    this.dashboard?.refresh(this.snapshot);
  }

  getTreeItem(element: CyclawTreeItem): vscode.TreeItem {
    return element;
  }

  getChildren(element?: CyclawTreeItem): CyclawTreeItem[] {
    if (element) {
      return element.children;
    }

    if (this.snapshot.error) {
      const item = new CyclawTreeItem(
        "cyClaw 读取失败",
        vscode.TreeItemCollapsibleState.None,
        "leaf"
      );
      item.description = this.snapshot.error;
      item.iconPath = new vscode.ThemeIcon("warning");
      return [item];
    }

    return [
      this.taskSection(),
      this.overviewSection(),
      this.modeSection(),
      this.activitySection(),
      this.statusSection(),
      this.modelSection(),
      this.permissionSection(),
      this.pendingSection(),
      this.patchesSection(),
      this.factPatchesSection(),
      this.nextStepsSection()
    ];
  }

  private taskSection(): CyclawTreeItem {
    const task = this.snapshot.activeTask;
    const children = task
      ? [
          leaf(task.title, "target"),
          leaf(`目标：${task.objective}`, "list-tree"),
          leaf(`决策 ${task.decisions.length} · 失败方案 ${task.failed_approaches.length}`, "symbol-event"),
          leaf(`检查点 ${task.checkpoints.length} · 上下文 ${this.snapshot.taskContext?.estimated_tokens ?? 0} Token`, "history")
        ]
      : [leaf("当前没有活动任务", "circle-outline")];
    const section = new CyclawTreeItem(
      "当前任务",
      vscode.TreeItemCollapsibleState.Expanded,
      "section",
      undefined,
      children
    );
    section.description = task ? "进行中" : "未开始";
    return section;
  }

  private modeSection(): CyclawTreeItem {
    const permissions = this.snapshot.policy?.permissions;
    const hasModel = Boolean(this.snapshot.models?.active_provider);
    const mode = resolveMode(permissions, hasModel);
    const children = [commandLeaf("选择运行策略", "settings", "cyclaw.configureMode", "选择 cyClaw 运行策略")];
    const section = new CyclawTreeItem("运行策略", vscode.TreeItemCollapsibleState.Expanded, "section", undefined, children);
    section.description = mode;
    section.tooltip = "运行策略是常用权限组合；高级权限用于逐项自定义。";
    return section;
  }

  private activitySection(): CyclawTreeItem {
    const children = this.snapshot.events.length
      ? this.snapshot.events.slice(0, 8).map((event) => {
          const time = new Date(event.created_at).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
          const item = leaf(event.summary, activityIcon(event.event_type));
          item.description = time;
          item.tooltip = `${new Date(event.created_at).toLocaleString("zh-CN")}\n${JSON.stringify(event.data, null, 2)}`;
          return item;
        })
      : [leaf("暂无运行活动", "history")];
    const section = new CyclawTreeItem("最近活动", vscode.TreeItemCollapsibleState.Expanded, "section", undefined, children);
    section.description = this.snapshot.events.length ? `${this.snapshot.events.length} 条` : "等待运行";
    return section;
  }

  private overviewSection(): CyclawTreeItem {
    const status = this.snapshot.status;
    const permissions = this.snapshot.policy?.permissions;
    const candidateCount = this.snapshot.candidates.length;
    const patchCount = this.snapshot.patches.length;
    const autoDocs = Boolean(
      permissions?.allow_docs_apply && permissions.allow_auto_apply_docs
    );
    const children = [
      commandLeaf("立即检查项目变更", "sync", "cyclaw.watchOnce", "检查一次项目变更"),
      commandLeaf("运行一次知识 Agent", "sparkle", "cyclaw.runAgent", "运行一次 cyClaw Agent"),
      commandLeaf("批量处理知识候选", "checklist", "cyclaw.batchCandidates", "批量处理知识候选"),
      commandLeaf("查看运行日志", "output", "cyclaw.showOutput", "打开并聚焦 cyClaw 运行日志"),
      leaf(`事件监听：${watchProcess ? "运行中" : "已停止"}`, watchProcess ? "radio-tower" : "debug-stop"),
      leaf(
        `待处理知识 ${candidateCount} · 文档草稿 ${patchCount}`,
        candidateCount || patchCount ? "warning" : "pass"
      ),
      leaf(
        autoDocs ? "自动文档管理：运行中" : "自动文档管理：需授权",
        autoDocs ? "zap" : "lock"
      )
    ];
    const section = new CyclawTreeItem(
      "运行概览",
      vscode.TreeItemCollapsibleState.Expanded,
      "section",
      undefined,
      children
    );
    section.description = status?.git_has_changes ? "发现变更" : "已同步";
    section.tooltip = "这里显示 cyClaw 当前发现的知识资产和自动化状态。";
    return section;
  }

  private modelSection(): CyclawTreeItem {
    const models = this.snapshot.models;
    const entries = models ? Object.entries(models.providers) : [];
    const children = entries.length
      ? entries.map(([name, provider]) => {
          const active = models?.active_provider === name;
          const item = leaf(
            `${active ? "当前" : "可切换"}: ${name}`,
            active ? "check" : "symbol-interface"
          );
          item.description = `${provider.model}${provider.thinking_enabled ? " · 思考开启" : ""}`;
          item.tooltip = `地址: ${provider.base_url}\nAPI Key: VS Code 安全存储\n环境变量: ${provider.api_key_env}`;
          item.command = { command: "cyclaw.switchModel", title: "切换活动模型" };
          return item;
        })
      : [commandLeaf("配置第一个模型 Provider", "add", "cyclaw.configureModel", "配置模型 Provider")];
    const section = new CyclawTreeItem(
      "模型 Provider",
      vscode.TreeItemCollapsibleState.Expanded,
      "section",
      undefined,
      children
    );
    section.description = entries.length ? `${entries.length} 个 · 点击切换` : "未配置";
    section.tooltip = "模型地址和名称属于当前项目；API Key 只保存在 VS Code 的安全存储。";
    section.command = { command: "cyclaw.configureModel", title: "配置模型 Provider" };
    return section;
  }

  private permissionSection(): CyclawTreeItem {
    const permissions = this.snapshot.policy?.permissions;
    const children = [
      permissionLeaf(
        "本地知识维护",
        "读取项目并写入 .cyclaw 知识资产。该基础能力始终启用。",
        true
      ),
      permissionLeaf("文档写入", "允许将已确认的文档草稿写入 docs。点击切换。", permissions?.allow_docs_apply, "docs"),
      permissionLeaf("模型 API 与联网", "允许使用 API Key 调用模型服务。点击切换。", Boolean(permissions?.allow_model_call && permissions?.allow_network), "model"),
      permissionLeaf("自动管理文档", "高风险：自动生成并写入 docs，不再等待人工确认；需要同时开启文档写入。默认关闭。", Boolean(permissions?.allow_docs_apply && permissions?.allow_auto_apply_docs), "autoDocs"),
      permissionLeaf("Shell 自动化", "允许 Agent 执行 Shell 自动化命令。点击切换。", permissions?.allow_shell, "shell"),
      permissionLeaf("源码写入", "允许 Agent 写入源码，风险较高。点击切换。", permissions?.allow_code_write, "code")
    ];
    const section = new CyclawTreeItem(
      "权限控制",
      vscode.TreeItemCollapsibleState.Collapsed,
      "section",
      undefined,
      children
    );
    section.description = "高级权限";
    section.tooltip = "运行策略会自动组合这些底层权限；这里用于逐项自定义。启用高风险能力时会要求确认。";
    section.command = { command: "cyclaw.configurePermissions", title: "管理高级权限" };
    return section;
  }

  private statusSection(): CyclawTreeItem {
    const status = this.snapshot.status;
    const children = status
      ? [
          leaf(`已初始化: ${status.initialized ? "是" : "否"}`, "repo"),
          leaf(`Git 未提交变更: ${status.git_has_changes ? "是" : "否"}`, "git-compare"),
          leaf(`待处理候选知识: ${status.inbox_pending}`, "inbox"),
          leaf(`待应用文档草稿: ${status.draft_pending}`, "diff"),
          leaf(`待应用事实草稿: ${status.fact_patch_pending}`, "database"),
          leaf(`可撤销事实草稿: ${status.fact_patch_revertible}`, "history"),
          leaf(`本地索引: ${status.index_exists ? "是" : "否"}`, "database")
        ]
      : [leaf("暂无状态，请先刷新", "info")];

    const section = new CyclawTreeItem(
      "项目状态",
      vscode.TreeItemCollapsibleState.Expanded,
      "section",
      undefined,
      children
    );
    section.description = status?.initialized ? "已连接" : "等待初始化";
    return section;
  }

  private pendingSection(): CyclawTreeItem {
    const children = this.snapshot.candidates.length
      ? this.snapshot.candidates.map((candidate) => {
          const item = leaf(candidate.summary, "lightbulb", "candidate", candidate.id);
          item.contextValue = "cyclawCandidate";
          item.description = `${candidate.confidence}% · ${candidate.recommended_doc}`;
          item.tooltip = [
            `ID: ${candidate.id}`,
            `重要性: ${candidate.importance}`,
            `置信度: ${candidate.confidence}%${candidate.reviewed_by_model ? "（模型已审查）" : "（本地规则）"}`,
            `建议文档: ${candidate.recommended_doc}`,
            `关联文件: ${candidate.related_files.join(", ")}`
          ].join("\n");
          item.command = { command: "cyclaw.openCandidate", title: "查看候选详情", arguments: [item] };
          return item;
        })
      : [leaf("暂无待处理候选知识", "pass")];

    const section = new CyclawTreeItem(
      "知识收件箱",
      vscode.TreeItemCollapsibleState.Expanded,
      "section",
      undefined,
      children
    );
    section.description = this.snapshot.candidates.length ? `${this.snapshot.candidates.length} 项待处理` : "已清空";
    return section;
  }

  private patchesSection(): CyclawTreeItem {
    const children = this.snapshot.patches.length
      ? this.snapshot.patches.map((patch) => {
          const item = leaf(patch.summary, "diff", "patch", patch.id);
          item.contextValue = "cyclawPatch";
          item.description = `${knowledgeOperationLabel(patch.operation)} · ${patch.target_doc}`;
          item.tooltip = [
            `ID: ${patch.id}`,
            `知识操作: ${knowledgeOperationLabel(patch.operation)}`,
            `候选知识: ${patch.candidate_id}`,
            `章节: ${patch.selector ?? "无"}`
          ].join("\n");
          item.command = { command: "cyclaw.openPatch", title: "查看文档 Diff", arguments: [item] };
          return item;
        })
      : [leaf("暂无待应用文档草稿", "pass")];

    const section = new CyclawTreeItem(
      "文档草稿",
      vscode.TreeItemCollapsibleState.Collapsed,
      "section",
      undefined,
      children
    );
    section.description = this.snapshot.patches.length ? `${this.snapshot.patches.length} 项待应用` : "已清空";
    return section;
  }

  private factPatchesSection(): CyclawTreeItem {
    const children = this.snapshot.factPatches.length ? [...this.snapshot.factPatches].sort((left, right) => factPatchStatusOrder(left.status) - factPatchStatusOrder(right.status)).map((patch) => {
      const item = leaf(`${knowledgeOperationLabel(patch.operation)} · ${factPatchStatusLabel(patch.status)}`, "database", "factPatch", patch.id);
      item.contextValue = "cyclawFactPatch";
      item.description = patch.target_fact_id ?? patch.after[0]?.id ?? "新事实";
      item.tooltip = `ID: ${patch.id}\n操作: ${knowledgeOperationLabel(patch.operation)}\n状态: ${factPatchStatusLabel(patch.status)}\n预览指纹: ${patch.preview_fingerprint}`;
      item.command = { command: "cyclaw.openFactPatch", title: "预览事实草稿", arguments: [item] };
      return item;
    }) : [leaf("暂无事实治理草稿", "pass")];
    return new CyclawTreeItem("事实治理草稿", vscode.TreeItemCollapsibleState.Collapsed, "section", undefined, children);
  }

  private nextStepsSection(): CyclawTreeItem {
    const steps = this.snapshot.status?.suggested_next_steps ?? [];
    const children = steps.length
      ? steps.map((step) => leaf(step, "arrow-right"))
      : [leaf("暂无建议", "pass")];

    return new CyclawTreeItem(
      "建议下一步",
      vscode.TreeItemCollapsibleState.Collapsed,
      "section",
      undefined,
      children
    );
  }
}

class CyclawDashboardProvider implements vscode.WebviewViewProvider {
  private view?: vscode.WebviewView;
  private snapshot: KnowledgeSnapshot = { candidates: [], patches: [], factPatches: [], events: [] };

  constructor(private readonly context: vscode.ExtensionContext) {}

  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.onDidReceiveMessage(async (message) => {
      const commands = new Set([
        "cyclaw.runAgent",
        "cyclaw.watchOnce",
        "cyclaw.configureMode",
        "cyclaw.batchCandidates",
        "cyclaw.configureModel",
        "cyclaw.configurePermissions",
        "cyclaw.showOutput",
        "cyclaw.startWatch",
        "cyclaw.stopWatch",
        "cyclaw.beginTask",
        "cyclaw.closeTask",
        "cyclaw.recordDecision",
        "cyclaw.reconcileKnowledge"
      ]);
      if (commands.has(message.command)) {
        await vscode.commands.executeCommand(message.command);
      } else if (message.command === "openCandidate" && typeof message.id === "string") {
        await vscode.commands.executeCommand("cyclaw.openCandidateById", message.id);
      } else if (message.command === "openPatch" && typeof message.id === "string") {
        await vscode.commands.executeCommand("cyclaw.openPatchById", message.id);
      } else if (message.command === "openFactPatch" && typeof message.id === "string") {
        const item = new CyclawTreeItem("事实草稿", vscode.TreeItemCollapsibleState.None, "factPatch", message.id);
        await vscode.commands.executeCommand("cyclaw.openFactPatch", item);
      }
    });
    this.render();
  }

  refresh(snapshot: KnowledgeSnapshot): void {
    this.snapshot = snapshot;
    this.render();
  }

  private render(): void {
    if (!this.view) return;
    this.view.webview.html = renderDashboardHtml(this.snapshot, this.context);
  }
}

let watchProcess: cp.ChildProcessWithoutNullStreams | undefined;
let outputChannel: vscode.OutputChannel;
let statusBarItem: vscode.StatusBarItem;
let extensionRoot: string;
let extensionContext: vscode.ExtensionContext;

export function activate(context: vscode.ExtensionContext): void {
  extensionContext = context;
  extensionRoot = context.extensionPath;
  outputChannel = vscode.window.createOutputChannel("cyClaw");
  statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 20);
  statusBarItem.command = "cyclaw.runAgent";
  statusBarItem.tooltip = "运行 cyClaw Agent 并刷新项目知识状态";
  statusBarItem.show();
  const provider = new CyclawKnowledgeProvider();
  const dashboard = new CyclawDashboardProvider(context);
  provider.attachDashboard(dashboard);

  context.subscriptions.push(outputChannel, statusBarItem);
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("cyclaw.knowledgeView", provider)
  );
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider("cyclaw.dashboardView", dashboard, {
      webviewOptions: { retainContextWhenHidden: true }
    })
  );
  context.subscriptions.push(
    vscode.workspace.onDidGrantWorkspaceTrust(() => void bootstrapWorkspace(provider))
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("cyclaw.refresh", () => refresh(provider)),
    vscode.commands.registerCommand("cyclaw.init", () => runCliCommand(["init"]).then(() => refresh(provider))),
    vscode.commands.registerCommand("cyclaw.scan", () => runCliCommand(["scan"]).then(() => refresh(provider))),
    vscode.commands.registerCommand("cyclaw.watchOnce", () =>
      runCliCommand(["watch", "--once", "--interval", "0"]).then(() => refresh(provider))
    ),
    vscode.commands.registerCommand("cyclaw.startWatch", () => startWatch(provider)),
    vscode.commands.registerCommand("cyclaw.stopWatch", () => stopWatch(provider)),
    vscode.commands.registerCommand("cyclaw.acceptCandidate", (item: CyclawTreeItem) =>
      updateCandidate(item, "accept", provider)
    ),
    vscode.commands.registerCommand("cyclaw.acceptAndDraft", (item: CyclawTreeItem) =>
      acceptAndDraft(item, provider)
    ),
    vscode.commands.registerCommand("cyclaw.ignoreCandidate", (item: CyclawTreeItem) =>
      updateCandidate(item, "ignore", provider)
    ),
    vscode.commands.registerCommand("cyclaw.applyPatch", (item: CyclawTreeItem) =>
      applyPatch(item, provider)
    ),
    vscode.commands.registerCommand("cyclaw.openCandidate", (item: CyclawTreeItem) =>
      openCandidate(item, provider)
    ),
    vscode.commands.registerCommand("cyclaw.openCandidateById", (id: string) =>
      openCandidate(new CyclawTreeItem("候选详情", vscode.TreeItemCollapsibleState.None, "candidate", id), provider)
    ),
    vscode.commands.registerCommand("cyclaw.openPatch", (item: CyclawTreeItem) =>
      openPatch(item, provider)
    ),
    vscode.commands.registerCommand("cyclaw.openPatchById", (id: string) =>
      openPatch(new CyclawTreeItem("文档草稿", vscode.TreeItemCollapsibleState.None, "patch", id), provider)
    ),
    vscode.commands.registerCommand("cyclaw.openFactPatch", (item: CyclawTreeItem) => openFactPatch(item, provider)),
    vscode.commands.registerCommand("cyclaw.configureModel", () => configureModel(provider)),
    vscode.commands.registerCommand("cyclaw.switchModel", () => switchModel(provider)),
    vscode.commands.registerCommand("cyclaw.configurePermissions", () => configurePermissions(provider)),
    vscode.commands.registerCommand("cyclaw.configureMode", () => configureMode(provider)),
    vscode.commands.registerCommand("cyclaw.batchCandidates", () => batchCandidates(provider)),
    vscode.commands.registerCommand("cyclaw.showOutput", () => showOutput()),
    vscode.commands.registerCommand("cyclaw.runAgent", () => runAgent(provider)),
    vscode.commands.registerCommand("cyclaw.beginTask", () => beginTask(provider)),
    vscode.commands.registerCommand("cyclaw.closeTask", () => closeActiveTask(provider)),
    vscode.commands.registerCommand("cyclaw.recordDecision", () => recordDecision(provider)),
    vscode.commands.registerCommand("cyclaw.reconcileKnowledge", () => reconcileKnowledge(provider)),
    vscode.commands.registerCommand("cyclaw.togglePermission", (item: CyclawTreeItem) =>
      togglePermission(item, provider)
    )
  );

  void bootstrapWorkspace(provider);
}

async function updateCandidate(
  item: CyclawTreeItem | undefined,
  action: "accept" | "ignore",
  provider: CyclawKnowledgeProvider
): Promise<void> {
  if (!item?.id || item.kind !== "candidate") {
    vscode.window.showWarningMessage("请先选择一个候选知识。");
    return;
  }

  await runCliCommand(["inbox", action, item.id]);
  await refresh(provider);
}

async function acceptAndDraft(
  item: CyclawTreeItem | undefined,
  provider: CyclawKnowledgeProvider
): Promise<void> {
  if (!item?.id || item.kind !== "candidate") return;
  try {
    await runCliCommand(["inbox", "accept", item.id]);
    await runCliCommand(["draft", "generate", "--candidate", item.id]);
    await refresh(provider);
    vscode.window.showInformationMessage("候选已接受并生成文档草稿，可点击草稿查看 Diff。");
  } catch (error) {
    vscode.window.showErrorMessage(`生成草稿失败：${error instanceof Error ? error.message : String(error)}`);
  }
}

async function openCandidate(
  item: CyclawTreeItem | undefined,
  provider: CyclawKnowledgeProvider
): Promise<void> {
  if (!item?.id) return;
  const candidate = provider.getCandidate(item.id);
  if (!candidate) return;
  const panel = vscode.window.createWebviewPanel(
    "cyclawCandidateDetail",
    "cyClaw 候选详情",
    vscode.ViewColumn.Active,
    { enableScripts: true }
  );
  panel.webview.html = renderCandidateHtml(candidate);
  panel.webview.onDidReceiveMessage(async (message) => {
    if (message.command === "draft") {
      await acceptAndDraft(item, provider);
      panel.dispose();
    } else if (message.command === "ignore") {
      await updateCandidate(item, "ignore", provider);
      panel.dispose();
    } else if (message.command === "source" && candidate.related_files[0]) {
      const root = workspaceRoot();
      if (root) {
        const uri = vscode.Uri.file(path.join(root, candidate.related_files[0]));
        await vscode.window.showTextDocument(uri, { preview: true });
      }
    }
  });
}

async function openPatch(
  item: CyclawTreeItem | undefined,
  provider: CyclawKnowledgeProvider
): Promise<void> {
  if (!item?.id) return;
  const patch = provider.getPatch(item.id);
  if (!patch) return;
  const original = await vscode.workspace.openTextDocument({ content: patch.original_content, language: "markdown" });
  const proposed = await vscode.workspace.openTextDocument({ content: patch.proposed_content, language: "markdown" });
  await vscode.commands.executeCommand(
    "vscode.diff",
    original.uri,
    proposed.uri,
    `cyClaw ${knowledgeOperationLabel(patch.operation)} · ${patch.target_doc}`,
    { preview: true }
  );
}

async function openFactPatch(item: CyclawTreeItem | undefined, provider: CyclawKnowledgeProvider): Promise<void> {
  if (!item?.id || item.kind !== "factPatch") return;
  const root = workspaceRoot();
  const patch = provider.getFactPatch(item.id);
  if (!root || !patch) return;
  const before = await vscode.workspace.openTextDocument({
    content: JSON.stringify({ operation: patch.operation, facts: patch.before }, null, 2),
    language: "json"
  });
  const after = await vscode.workspace.openTextDocument({
    content: JSON.stringify({ operation: patch.operation, facts: patch.after }, null, 2),
    language: "json"
  });
  await vscode.commands.executeCommand(
    "vscode.diff",
    before.uri,
    after.uri,
    `cyClaw ${knowledgeOperationLabel(patch.operation)} · ${patch.target_fact_id ?? patch.after[0]?.id ?? "新事实"}`,
    { preview: true }
  );
  const action = patch.status === "pending" ? "应用" : patch.status === "applied" ? "撤销" : undefined;
  const choice = action
    ? await vscode.window.showInformationMessage(`${knowledgeOperationLabel(patch.operation)}：${patch.before.map((fact) => fact.statement).join("；") || "新增"} -> ${patch.after.map((fact) => fact.statement).join("；")}`, action)
    : await vscode.window.showInformationMessage(`事实草稿状态：${factPatchStatusLabel(patch.status)}`);
  if (choice === "应用") await callMcpTool("apply_fact_patch", root, { patch_id: patch.id });
  if (choice === "撤销") await callMcpTool("revert_fact_patch", root, { patch_id: patch.id });
  await refresh(provider);
}

async function beginTask(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) return;
  const title = await vscode.window.showInputBox({
    prompt: "任务名称",
    placeHolder: "例如：调整退款状态机"
  });
  if (!title) return;
  const objective = await vscode.window.showInputBox({
    prompt: "任务目标",
    placeHolder: "描述需要完成的行为和边界"
  });
  if (!objective) return;
  const currentFile = vscode.window.activeTextEditor?.document.uri.fsPath;
  const relatedFiles = currentFile?.startsWith(root)
    ? [path.relative(root, currentFile).replaceAll("\\", "/")]
    : [];
  try {
    await callMcpTool("begin_task", root, {
      title,
      objective,
      related_files: relatedFiles,
      context_budget_tokens: 2000
    });
    await refresh(provider);
    vscode.window.showInformationMessage(`cyClaw 任务已开始：${title}`);
  } catch (error) {
    vscode.window.showErrorMessage(`无法开始任务：${error instanceof Error ? error.message : String(error)}`);
  }
}

async function closeActiveTask(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) return;
  const summary = await vscode.window.showInputBox({
    prompt: "任务完成摘要",
    placeHolder: "说明完成内容、剩余风险和后续事项"
  });
  if (!summary) return;
  try {
    await callMcpTool("close_task", root, { summary, reconcile: true });
    await refresh(provider);
    vscode.window.showInformationMessage("cyClaw 已关闭任务并完成知识对账。 ");
  } catch (error) {
    vscode.window.showErrorMessage(`无法关闭任务：${error instanceof Error ? error.message : String(error)}`);
  }
}

async function recordDecision(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) return;
  const statement = await vscode.window.showInputBox({
    prompt: "关键决策",
    placeHolder: "例如：退款完成后不得重新进入处理中"
  });
  if (!statement) return;
  const rationale = await vscode.window.showInputBox({
    prompt: "决策原因",
    placeHolder: "说明为什么采用该约束或方案"
  });
  if (!rationale) return;
  const currentFile = vscode.window.activeTextEditor?.document.uri.fsPath;
  const evidence = currentFile?.startsWith(root)
    ? [path.relative(root, currentFile).replaceAll("\\", "/")]
    : [];
  try {
    await callMcpTool("record_decision", root, {
      statement,
      rationale,
      evidence,
      confidence: 90
    });
    await refresh(provider);
    vscode.window.showInformationMessage("关键决策已写入项目 Fact Ledger。 ");
  } catch (error) {
    vscode.window.showErrorMessage(`无法记录决策：${error instanceof Error ? error.message : String(error)}`);
  }
}

async function reconcileKnowledge(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) return;
  try {
    const value = await callMcpTool<{ report: ReconciliationReport }>(
      "reconcile_project_knowledge",
      root
    );
    await refresh(provider);
    vscode.window.showInformationMessage(
      `知识对账完成：重复 ${value.report.duplicate_count}，冲突 ${value.report.conflict_count}，失效 ${value.report.stale_count}`
    );
  } catch (error) {
    vscode.window.showErrorMessage(`知识对账失败：${error instanceof Error ? error.message : String(error)}`);
  }
}

async function runAgent(provider: CyclawKnowledgeProvider): Promise<void> {
  try {
    const root = workspaceRoot();
    if (!root) return;
    const [policy, models] = await Promise.all([
      callMcpTool<PolicySnapshot>("get_policy", root),
      callMcpTool<ModelProvidersSnapshot>("get_model_providers", root)
    ]);
    const useModel = Boolean(
      models.active_provider && policy.permissions.allow_model_call && policy.permissions.allow_network
    );
    await runCliCommand(useModel
      ? ["agent", "run", "--once"]
      : ["agent", "run", "--once", "--no-model"]);
    await refresh(provider);
    vscode.window.showInformationMessage(`cyClaw 已完成一次项目知识分析${useModel ? "，并使用活动模型完成审查" : "（本地规则模式）"}。`);
  } catch (error) {
    vscode.window.showErrorMessage(`Agent 执行失败：${error instanceof Error ? error.message : String(error)}`);
  }
}

async function configureModel(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) {
    vscode.window.showWarningMessage("请先打开一个项目工作区。");
    return;
  }
  const name = await vscode.window.showInputBox({
    prompt: "Provider 名称，例如 deepseek 或 openai",
    validateInput: (value) => (/^[a-zA-Z0-9_-]+$/.test(value) ? undefined : "仅支持字母、数字、_ 和 -")
  });
  if (!name) return;
  const baseUrl = await vscode.window.showInputBox({
    prompt: "OpenAI-compatible API Base URL",
    placeHolder: "https://api.example.com/v1",
    validateInput: (value) => (value.startsWith("http://") || value.startsWith("https://") ? undefined : "请输入 http:// 或 https:// 地址")
  });
  if (!baseUrl) return;
  const model = await vscode.window.showInputBox({ prompt: "模型名称", placeHolder: "例如 deepseek-chat" });
  if (!model) return;
  const apiKey = await vscode.window.showInputBox({ prompt: "API Key", password: true, ignoreFocusOut: true });
  if (!apiKey) return;
  const thinking = await vscode.window.showQuickPick(
    [
      { label: "默认关闭", description: "直接响应，资源占用更低", value: false },
      { label: "开启思考", description: "仅在模型支持且你明确需要时使用", value: true }
    ],
    { placeHolder: "选择思考模式" }
  );
  if (!thinking) return;

  const apiKeyEnv = `CYCLAW_MODEL_API_KEY_${name.replace(/[^a-zA-Z0-9]/g, "_").toUpperCase()}`;
  const binding: ModelSecretBinding = { workspaceRoot: root, providerName: name, apiKeyEnv };
  await extensionContext.secrets.store(secretKey(binding), apiKey);
  await saveSecretBinding(binding);
  const args = [
    "model", "add", name,
    "--base-url", baseUrl,
    "--model", model,
    "--api-key-env", apiKeyEnv,
    "--active"
  ];
  if (thinking.value) args.push("--thinking");
  try {
    await runCliCommand(args);
    const verify = await vscode.window.showInformationMessage("模型已保存，是否立即测试连通性？", "测试", "稍后");
    if (verify === "测试") {
      await runCliCommand(["model", "test", name]);
      vscode.window.showInformationMessage(`模型 ${name} 连通性测试成功。`);
    }
    await refresh(provider);
  } catch (error) {
    vscode.window.showErrorMessage(`模型配置失败: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function switchModel(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) return;
  try {
    const models = await callMcpTool<ModelProvidersSnapshot>("get_model_providers", root);
    const choices = Object.entries(models.providers).map(([name, value]) => ({
      label: name,
      description: `${value.model} · ${value.base_url}`,
      detail: models.active_provider === name ? "当前活动模型" : "切换为活动模型",
      name
    }));
    const selected = await vscode.window.showQuickPick(choices, { placeHolder: "选择当前项目使用的模型 Provider" });
    if (!selected) return;
    await runCliCommand(["model", "use", selected.name]);
    await refresh(provider);
    vscode.window.showInformationMessage(`已切换到模型 Provider: ${selected.name}`);
  } catch (error) {
    vscode.window.showErrorMessage(`切换模型失败: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function configurePermissions(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) return;
  try {
    const policy = await callMcpTool<PolicySnapshot>("get_policy", root);
    const permissions = policy.permissions;
    const choices = [
      { label: "本地知识维护", description: "读取项目并写入 .cyclaw，本地基础能力", detail: "始终启用", action: "info" },
      { label: `文档写入：${permissionLabel(permissions.allow_docs_apply)}`, description: "允许将已确认的草稿写入 docs", action: "docs" },
      { label: `模型 API 与联网：${permissionLabel(permissions.allow_model_call && permissions.allow_network)}`, description: "允许使用 API Key 调用模型服务", action: "model" },
      { label: `自动管理文档：${permissionLabel(permissions.allow_auto_apply_docs)}`, description: "Agent 自动生成并应用文档，不再等待确认", detail: "高风险权限", action: "autoDocs" },
      { label: `Shell 自动化：${permissionLabel(permissions.allow_shell)}`, description: "允许执行 Shell 自动化命令", action: "shell" },
      { label: `源码写入：${permissionLabel(permissions.allow_code_write)}`, description: "允许 Agent 写入源码，风险最高", action: "code" }
    ];
    const selected = await vscode.window.showQuickPick(choices, { placeHolder: "高级权限：逐项调整运行策略底层的能力开关" });
    if (!selected || selected.action === "info") return;
    await togglePermissionAction(selected.action as PermissionAction, selected.label.split("：")[0], permissions, provider);
  } catch (error) {
    vscode.window.showErrorMessage(`权限更新失败: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function configureMode(provider: CyclawKnowledgeProvider): Promise<void> {
  const selected = await vscode.window.showQuickPick([
    { label: "观察模式", description: "只检测和收集，不调用模型，不写入项目文档", mode: "observe" },
    { label: "审阅模式", description: "生成候选和草稿，由用户确认后写入文档", mode: "review" },
    { label: "智能审阅", description: "使用活动模型审查候选，文档仍需用户确认", mode: "smart" },
    { label: "自动文档", description: "使用模型并自动写入 docs，不再逐次确认", detail: "高风险", mode: "auto" }
  ], { placeHolder: "选择运行策略：cyClaw 会自动配置对应的底层权限" });
  if (!selected) return;
  if (selected.mode === "auto") {
    const confirmed = await vscode.window.showWarningMessage(
      "自动文档模式会直接修改当前项目 docs/ 下的文档，是否继续？",
      { modal: true },
      "启用自动文档"
    );
    if (confirmed !== "启用自动文档") return;
    const threshold = await vscode.window.showInputBox({
      prompt: "自动写入文档所需的最低置信度",
      value: "90",
      validateInput: (value) => {
        const number = Number(value);
        return Number.isInteger(number) && number >= 0 && number <= 100 ? undefined : "请输入 0-100 的整数";
      }
    });
    if (!threshold) return;
    await runCliCommand(["policy", "set-auto-threshold", threshold]);
  }
  const docs = selected.mode !== "observe";
  const model = selected.mode === "smart" || selected.mode === "auto";
  const autoDocs = selected.mode === "auto";
  await runCliCommand(["policy", "set", "allow_docs_apply", `--enabled=${docs}`]);
  await runCliCommand(["policy", "set", "allow_model_call", `--enabled=${model}`]);
  await runCliCommand(["policy", "set", "allow_network", `--enabled=${model}`]);
  await runCliCommand(["policy", "set", "allow_auto_apply_docs", `--enabled=${autoDocs}`]);
  await refresh(provider);
  vscode.window.showInformationMessage(`cyClaw 运行策略已切换为${selected.label}。`);
}

async function batchCandidates(provider: CyclawKnowledgeProvider): Promise<void> {
  const candidates = provider.getCandidates();
  if (!candidates.length) {
    vscode.window.showInformationMessage("当前没有待处理知识候选。");
    return;
  }
  const selected = await vscode.window.showQuickPick([
    { label: "接受高置信度候选", description: "接受置信度不低于 80 的候选并生成草稿", action: "accept" },
    { label: "忽略低置信度候选", description: "忽略置信度低于 60 或模型建议忽略的候选", action: "ignore" }
  ], { placeHolder: `当前有 ${candidates.length} 个待处理候选` });
  if (!selected) return;
  const targets = selected.action === "accept"
    ? candidates.filter((candidate) => candidate.confidence >= 80 && candidate.model_recommendation !== "ignore")
    : candidates.filter((candidate) => candidate.confidence < 60 || candidate.model_recommendation === "ignore");
  if (!targets.length) {
    vscode.window.showInformationMessage("没有符合该批量规则的候选。");
    return;
  }
  for (const candidate of targets) {
    await runCliCommand(["inbox", selected.action, candidate.id]);
    if (selected.action === "accept") {
      try {
        await runCliCommand(["draft", "generate", "--candidate", candidate.id]);
      } catch (error) {
        outputChannel.appendLine(`候选 ${candidate.id} 未生成新草稿：${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }
  await refresh(provider);
  vscode.window.showInformationMessage(`已批量${selected.action === "accept" ? "接受并生成草稿" : "忽略"} ${targets.length} 个候选。`);
}

function resolveMode(
  permissions: PolicySnapshot["permissions"] | undefined,
  hasModel: boolean
): string {
  if (!permissions) return "读取中";
  const modelEnabled = permissions.allow_model_call && permissions.allow_network;
  if (permissions.allow_auto_apply_docs && permissions.allow_docs_apply && modelEnabled && hasModel) return "自动文档";
  if (!permissions.allow_auto_apply_docs && permissions.allow_docs_apply && modelEnabled && hasModel) return "智能审阅";
  if (!permissions.allow_auto_apply_docs && permissions.allow_docs_apply && !modelEnabled) return "审阅模式";
  if (!permissions.allow_auto_apply_docs && !permissions.allow_docs_apply && !modelEnabled) return "观察模式";
  return "自定义权限";
}

async function togglePermission(item: CyclawTreeItem | undefined, provider: CyclawKnowledgeProvider): Promise<void> {
  if (!item?.permissionAction) {
    vscode.window.showInformationMessage("本地知识维护是 cyClaw 的基础能力，无需单独授权。");
    return;
  }
  try {
    const policy = await callMcpTool<PolicySnapshot>("get_policy", workspaceRoot()!);
    await togglePermissionAction(item.permissionAction, item.label as string, policy.permissions, provider);
  } catch (error) {
    vscode.window.showErrorMessage(`权限更新失败: ${error instanceof Error ? error.message : String(error)}`);
  }
}

async function togglePermissionAction(
  action: PermissionAction,
  label: string,
  permissions: PolicySnapshot["permissions"],
  provider: CyclawKnowledgeProvider
): Promise<void> {
  const enable = permissionEnabled(action, permissions);
  if (enable) {
    const confirmed = await vscode.window.showWarningMessage(
      `确认授予“${label}”？授权将立即写入当前项目的 .cyclaw/config.yaml。`,
      { modal: true },
      "授予"
    );
    if (confirmed !== "授予") return;
  }
  await applyPermissionToggle(action, enable);
  await refresh(provider);
  vscode.window.showInformationMessage(`${label}已${enable ? "授予" : "撤销"}。`);
}

function permissionLabel(enabled: boolean): string {
  return enabled ? "已允许" : "未允许";
}

function permissionEnabled(action: string, permissions: PolicySnapshot["permissions"]): boolean {
  switch (action) {
    case "docs": return !permissions.allow_docs_apply;
    case "model": return !(permissions.allow_model_call && permissions.allow_network);
    case "autoDocs": return !permissions.allow_auto_apply_docs;
    case "shell": return !permissions.allow_shell;
    case "code": return !permissions.allow_code_write;
    default: return false;
  }
}

async function applyPermissionToggle(action: string, enable: boolean): Promise<void> {
  const value = `--enabled=${enable}`;
  switch (action) {
    case "docs":
      await runCliCommand(["policy", "set", "allow_docs_apply", value]);
      return;
    case "model":
      await runCliCommand(["policy", "set", "allow_model_call", value]);
      await runCliCommand(["policy", "set", "allow_network", value]);
      return;
    case "autoDocs":
      if (enable) {
        await runCliCommand(["policy", "set", "allow_docs_apply", value]);
      }
      await runCliCommand(["policy", "set", "allow_auto_apply_docs", value]);
      return;
    case "shell":
      await runCliCommand(["policy", "set", "allow_shell", value]);
      return;
    case "code":
      await runCliCommand(["policy", "set", "allow_code_write", value]);
  }
}

async function saveSecretBinding(binding: ModelSecretBinding): Promise<void> {
  const bindings = extensionContext.globalState.get<ModelSecretBinding[]>("modelSecretBindings", []);
  const next = bindings.filter(
    (item) => item.workspaceRoot !== binding.workspaceRoot || item.providerName !== binding.providerName
  );
  next.push(binding);
  await extensionContext.globalState.update("modelSecretBindings", next);
}

async function applyPatch(
  item: CyclawTreeItem | undefined,
  provider: CyclawKnowledgeProvider
): Promise<void> {
  if (!item?.id || item.kind !== "patch") {
    vscode.window.showWarningMessage("请先选择一个文档草稿。");
    return;
  }

  try {
    await runCliCommand(["draft", "apply", item.id]);
    await refresh(provider);
    const action = await vscode.window.showInformationMessage(
      `文档草稿已应用: ${item.description ?? item.id}`,
      "查看文件",
      "撤销"
    );
    if (action === "撤销") {
      await runCliCommand(["draft", "revert", item.id]);
      await refresh(provider);
      vscode.window.showInformationMessage("本次文档修改已撤销。");
    } else if (action === "查看文件" && typeof item.description === "string") {
      const root = workspaceRoot();
      if (root) await vscode.window.showTextDocument(vscode.Uri.file(path.join(root, item.description)));
    }
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    const message = detail.includes("allow_docs_apply") || detail.includes("权限拒绝")
      ? "无法应用草稿：当前项目未授予“文档写入”权限。请点击“权限控制 > 文档写入”后授予权限。"
      : `无法应用文档草稿：${detail}`;
    vscode.window.showErrorMessage(message);
  }
}

export function deactivate(): void {
  stopWatch();
}

async function bootstrapWorkspace(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) {
    provider.refresh({ candidates: [], patches: [], factPatches: [], events: [], error: "未打开工作区" });
    return;
  }
  if (!vscode.workspace.isTrusted) {
    provider.refresh({ candidates: [], patches: [], factPatches: [], events: [], error: "工作区未受信任，cyClaw 不会启动 CLI 或读取项目文件。" });
    statusBarItem.text = "$(shield) cyClaw 等待工作区信任";
    return;
  }

  try {
    const status = await callMcpTool<ProjectStatus>("get_project_status", root);
    const config = workspaceConfiguration(root);
    if (!status.initialized && config.get<boolean>("autoInitialize", true)) {
      outputChannel.appendLine(`首次打开项目，初始化 cyClaw: ${root}`);
      await runCliCommand(["init"]);
      await runCliCommand(["scan"]);
    }
    await refresh(provider);
    if (config.get<boolean>("autoWatch", true)) {
      await startWatch(provider);
    }
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    outputChannel.appendLine(`cyClaw 自动启动失败: ${message}`);
    provider.refresh({ candidates: [], patches: [], factPatches: [], events: [], error: message });
  }
}

async function refresh(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) {
    provider.refresh({
      candidates: [],
      patches: [],
      factPatches: [],
      events: [],
      error: "未打开工作区"
    });
    return;
  }

  try {
    const [status, pending, patches, factPatches, policy, models, events, active, reconciliation] = await Promise.all([
      callMcpTool<ProjectStatus>("get_project_status", root),
      callMcpTool<{ candidates: KnowledgeCandidate[] }>("list_pending_knowledge", root),
      callMcpTool<{ patches: DocumentPatch[] }>("list_document_patches", root),
      callMcpTool<{ patches: FactPatch[] }>("list_fact_patches", root, { limit: 100 }),
      callMcpTool<PolicySnapshot>("get_policy", root),
      callMcpTool<ModelProvidersSnapshot>("get_model_providers", root),
      callMcpTool<{ events: AgentEvent[] }>("list_events", root, { limit: 12 }),
      callMcpTool<{ task?: TaskRecord }>("get_active_task", root),
      callMcpTool<{ report?: ReconciliationReport }>("get_latest_reconciliation", root)
    ]);
    const taskContext = active.task
      ? await callMcpTool<TaskContextPack>("get_task_context", root, {
          task_id: active.task.id,
          budget_tokens: active.task.context_budget_tokens,
          limit: 10
        })
      : undefined;

    provider.refresh({
      status,
      candidates: pending.candidates,
      patches: patches.patches,
      factPatches: factPatches.patches,
      policy,
      models,
      events: events.events,
      activeTask: active.task,
      taskContext,
      reconciliation: reconciliation.report
    });
    updateStatusBar({
      status,
      candidates: pending.candidates.length,
      patches: patches.patches.length,
      factPatches: status.fact_patch_pending,
      activeTask: active.task
    });
  } catch (error) {
    provider.refresh({
      candidates: [],
      patches: [],
      factPatches: [],
      events: [],
      error: error instanceof Error ? error.message : String(error)
    });
    statusBarItem.text = "$(warning) cyClaw 读取失败";
    statusBarItem.backgroundColor = new vscode.ThemeColor("statusBarItem.errorBackground");
  }
}

function updateStatusBar(value: { status: ProjectStatus; candidates: number; patches: number; factPatches: number; activeTask?: TaskRecord }): void {
  const pending = value.candidates + value.patches + value.factPatches;
  statusBarItem.text = value.activeTask
    ? `$(target) cyClaw · ${value.activeTask.title}`
    : pending > 0
    ? `$(book) cyClaw ${value.candidates} 候选 · ${value.patches} 文档 · ${value.factPatches} 事实`
    : "$(check) cyClaw 已同步";
  statusBarItem.backgroundColor = pending > 0
    ? new vscode.ThemeColor("statusBarItem.warningBackground")
    : undefined;
  statusBarItem.tooltip = value.activeTask
    ? `当前任务: ${value.activeTask.title}\n目标: ${value.activeTask.objective}\n决策: ${value.activeTask.decisions.length}\n失败方案: ${value.activeTask.failed_approaches.length}`
    : pending > 0
    ? `项目已连接\n待处理候选知识: ${value.candidates}\n待应用文档草稿: ${value.patches}\n待应用事实草稿: ${value.factPatches}\n点击运行 Agent`
    : "项目知识状态已同步，点击运行 Agent";
}

async function startWatch(provider: CyclawKnowledgeProvider): Promise<void> {
  const root = workspaceRoot();
  if (!root) {
    vscode.window.showWarningMessage("请先打开一个项目工作区。");
    return;
  }
  ensureTrustedWorkspace();

  if (watchProcess) {
    vscode.window.showInformationMessage("cyClaw watch 已在运行。");
    return;
  }

  const debounce = workspaceConfiguration(root).get<number>("watchDebounceMilliseconds", 600);
  const command = await resolveCommand(["watch", "--debounce-ms", String(debounce)], root);
  watchProcess = cp.spawn(command.file, command.args, { cwd: command.cwd, env: command.env });
  provider.notify();
  outputChannel.appendLine(`cyClaw 事件驱动 Watch 已启动，合并窗口 ${debounce}ms。`);

  watchProcess.stdout.on("data", (chunk: Buffer) => {
    outputChannel.append(chunk.toString());
    refresh(provider);
  });
  watchProcess.stderr.on("data", (chunk: Buffer) => outputChannel.append(chunk.toString()));
  watchProcess.on("exit", (code) => {
    outputChannel.appendLine(`cyClaw watch 已退出，退出码: ${code ?? "未知"}`);
    watchProcess = undefined;
    provider.notify();
    refresh(provider);
  });
}

function stopWatch(provider?: CyclawKnowledgeProvider): void {
  if (!watchProcess) {
    return;
  }

  watchProcess.kill();
  watchProcess = undefined;
  provider?.notify();
  outputChannel.appendLine("cyClaw watch 已停止。");
}

function showOutput(): void {
  outputChannel.appendLine(`[${new Date().toLocaleTimeString("zh-CN")}] 已打开 cyClaw 运行日志。`);
  outputChannel.show(false);
}

async function runCliCommand(args: string[]): Promise<void> {
  const root = workspaceRoot();
  if (!root) {
    vscode.window.showWarningMessage("请先打开一个项目工作区。");
    return;
  }
  ensureTrustedWorkspace();

  const command = await resolveCommand(args, root);
  outputChannel.appendLine(`> ${command.file} ${command.args.join(" ")}`);

  await new Promise<void>((resolve, reject) => {
    const child = cp.spawn(command.file, command.args, { cwd: command.cwd, env: command.env });
    let stderr = "";
    child.stdout.on("data", (chunk: Buffer) => outputChannel.append(chunk.toString()));
    child.stderr.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      stderr += text;
      outputChannel.append(text);
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(stderr.trim() || `cyClaw 命令执行失败，退出码: ${code}`));
      }
    });
  });
}

async function callMcpTool<T>(toolName: McpToolName, root: string, argumentsValue: Record<string, unknown> = {}): Promise<T> {
  ensureTrustedWorkspace();
  const command = await resolveCommand(["mcp"], root);
  const initialize = {
    jsonrpc: "2.0",
    id: 1,
    method: "initialize",
    params: {}
  };
  const call = {
    jsonrpc: "2.0",
    id: 2,
    method: "tools/call",
    params: {
      name: toolName,
      arguments: argumentsValue
    }
  };
  const input = frameMessage(initialize) + frameMessage(call);

  const output = await runProcess(command.file, command.args, command.cwd, input, command.env);
  const messages = parseFrames(output.stdout);
  const response = messages.find((message) => message.id === 2);
  if (!response) {
    throw new Error(`MCP 工具没有返回响应: ${toolName}`);
  }
  if (response.error) {
    throw new Error(response.error.message ?? `MCP 工具调用失败: ${toolName}`);
  }

  const text = response.result?.content?.[0]?.text;
  if (typeof text !== "string") {
    throw new Error(`MCP 工具返回格式不正确: ${toolName}`);
  }

  return JSON.parse(text) as T;
}

function ensureTrustedWorkspace(): void {
  if (!vscode.workspace.isTrusted) {
    throw new Error("当前 VS Code 工作区未受信任，cyClaw 已阻止执行本地 CLI。请先信任工作区。");
  }
}

function runProcess(
  file: string,
  args: string[],
  cwd: string,
  input: string,
  env: NodeJS.ProcessEnv
): Promise<{ stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    const child = cp.spawn(file, args, { cwd, env });
    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk: Buffer) => {
      stdout += chunk.toString();
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString();
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) {
        resolve({ stdout, stderr });
      } else {
        reject(new Error(stderr || `进程执行失败，退出码: ${code}`));
      }
    });

    child.stdin.write(input);
    child.stdin.end();
  });
}

async function resolveCommand(
  args: string[],
  root: string
): Promise<{ file: string; args: string[]; cwd: string; env: NodeJS.ProcessEnv }> {
  const config = workspaceConfiguration(root);
  const useCargoRun = config.get<boolean>("useCargoRun", false);
  const env = await commandEnvironment(root);
  if (useCargoRun) {
    return {
      file: "cargo",
      args: ["run", "-p", "cyclaw-cli", "--", ...args, "--path", root],
      cwd: repositoryRoot(root),
      env
    };
  }

  const configuredCommand = config.get<string>("command", "").trim();
  return {
    file: configuredCommand || bundledCliPath(),
    args: [...args, "--path", root],
    cwd: root,
    env
  };
}

function workspaceConfiguration(root: string): vscode.WorkspaceConfiguration {
  return vscode.workspace.getConfiguration("cyclaw", vscode.Uri.file(root));
}

function bundledCliPath(): string {
  if (process.platform !== "win32" || process.arch !== "x64") {
    throw new Error("当前 VSIX 仅内置 Windows x64 CLI，请在设置中配置 cyclaw.command。");
  }
  const cliPath = path.join(extensionRoot, "bin", "win32-x64", "cyclaw.exe");
  if (!fs.existsSync(cliPath)) {
    throw new Error("未找到内置 cyClaw CLI，请重新安装完整 VSIX，或在设置中配置 cyclaw.command。");
  }
  return cliPath;
}

async function commandEnvironment(root: string): Promise<NodeJS.ProcessEnv> {
  const environment: NodeJS.ProcessEnv = { ...process.env };
  const bindings = extensionContext.globalState.get<ModelSecretBinding[]>("modelSecretBindings", []);
  for (const binding of bindings.filter((item) => item.workspaceRoot === root)) {
    const apiKey = await extensionContext.secrets.get(secretKey(binding));
    if (apiKey) {
      environment[binding.apiKeyEnv] = apiKey;
    }
  }
  return environment;
}

function secretKey(binding: ModelSecretBinding): string {
  return `cyclaw.model.${encodeURIComponent(binding.workspaceRoot)}.${binding.providerName}`;
}

function repositoryRoot(workspace: string): string {
  return path.resolve(workspace);
}

function workspaceRoot(): string | undefined {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
}

function frameMessage(value: unknown): string {
  const body = JSON.stringify(value);
  const length = Buffer.byteLength(body, "utf8");
  return `Content-Length: ${length}\r\n\r\n${body}`;
}

function parseFrames(output: string): Array<Record<string, any>> {
  const messages: Array<Record<string, any>> = [];
  let cursor = 0;

  while (cursor < output.length) {
    const headerEnd = output.indexOf("\r\n\r\n", cursor);
    if (headerEnd === -1) {
      break;
    }

    const header = output.slice(cursor, headerEnd);
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match) {
      break;
    }

    const length = Number(match[1]);
    const bodyStart = headerEnd + 4;
    const body = output.slice(bodyStart, bodyStart + length);
    messages.push(JSON.parse(body));
    cursor = bodyStart + length;
  }

  return messages;
}

function leaf(
  label: string,
  icon: string,
  kind: "leaf" | "candidate" | "patch" | "factPatch" | "permission" = "leaf",
  id?: string
): CyclawTreeItem {
  const item = new CyclawTreeItem(label, vscode.TreeItemCollapsibleState.None, kind, id);
  item.iconPath = new vscode.ThemeIcon(icon);
  return item;
}

function commandLeaf(label: string, icon: string, command: string, title: string): CyclawTreeItem {
  const item = leaf(label, icon);
  item.command = { command, title };
  return item;
}

function permissionLeaf(
  label: string,
  tooltip: string,
  enabled: boolean | undefined,
  action?: PermissionAction
): CyclawTreeItem {
  const item = new CyclawTreeItem(
    label,
    vscode.TreeItemCollapsibleState.None,
    "permission",
    undefined,
    [],
    action
  );
  item.iconPath = new vscode.ThemeIcon(enabled ? "pass-filled" : "circle-slash");
  item.contextValue = action ? "cyclawPermission" : "cyclawPermissionFixed";
  item.description = enabled ? "已允许" : "需授权";
  item.tooltip = tooltip;
  if (action) {
    item.command = { command: "cyclaw.togglePermission", title: "切换权限", arguments: [item] };
  }
  return item;
}

function renderDashboardHtml(snapshot: KnowledgeSnapshot, context: vscode.ExtensionContext): string {
  const nonce = Math.random().toString(36).slice(2);
  const status = snapshot.status;
  const mode = resolveMode(snapshot.policy?.permissions, Boolean(snapshot.models?.active_provider));
  const watching = Boolean(watchProcess);
  const candidates = snapshot.candidates.slice(0, 4).map((candidate) => `
    <button class="work-item" data-kind="candidate" data-id="${escapeHtml(candidate.id)}">
      <span class="work-main"><strong>${escapeHtml(candidate.summary)}</strong><small>${escapeHtml(candidate.recommended_doc)}</small></span>
      <span class="score ${candidate.confidence >= 80 ? "high" : candidate.confidence < 60 ? "low" : "medium"}">${candidate.confidence}%</span>
    </button>`).join("");
  const patches = snapshot.patches.slice(0, 3).map((patch) => `
    <button class="work-item" data-kind="patch" data-id="${escapeHtml(patch.id)}">
      <span class="work-main"><strong>${escapeHtml(patch.summary)}</strong><small>${escapeHtml(patch.target_doc)}</small></span>
      <span class="tag">${escapeHtml(knowledgeOperationLabel(patch.operation))}</span>
    </button>`).join("");
  const factPatches = snapshot.factPatches.filter((patch) => patch.status !== "reverted").slice(0, 3).map((patch) => `
    <button class="work-item" data-kind="factPatch" data-id="${escapeHtml(patch.id)}">
      <span class="work-main"><strong>${escapeHtml(patch.after[0]?.statement ?? patch.before[0]?.statement ?? patch.id)}</strong><small>${escapeHtml(factPatchStatusLabel(patch.status))}</small></span>
      <span class="tag">${escapeHtml(knowledgeOperationLabel(patch.operation))}</span>
    </button>`).join("");
  const task = snapshot.activeTask;
  const contextFacts = snapshot.taskContext?.facts.facts.slice(0, 3).map((item) => `
    <li><strong>${escapeHtml(item.fact.statement)}</strong><small>${escapeHtml(item.relevance_reason)} · ${item.fact.confidence}%</small></li>`).join("") ?? "";
  const taskSection = task ? `
<section class="task-band">
  <div class="task-heading"><div><span>当前任务</span><h2>${escapeHtml(task.title)}</h2></div><span class="tag active">进行中</span></div>
  <p>${escapeHtml(task.objective)}</p>
  <div class="task-metrics"><span><strong>${task.decisions.length}</strong> 决策</span><span><strong>${task.failed_approaches.length}</strong> 失败方案</span><span><strong>${task.checkpoints.length}</strong> 检查点</span><span><strong>${snapshot.taskContext?.estimated_tokens ?? 0}</strong> Token</span></div>
  ${contextFacts ? `<h3>相关项目事实</h3><ul class="context-facts">${contextFacts}</ul>` : '<div class="empty compact">尚未召回相关历史事实。</div>'}
  <div class="task-actions"><button class="action primary" data-command="cyclaw.recordDecision">记录决策</button><button class="action" data-command="cyclaw.reconcileKnowledge">知识对账</button><button class="action" data-command="cyclaw.closeTask">关闭任务</button></div>
</section>` : `
<section class="task-band empty-task">
  <div><span>项目记忆</span><h2>当前没有活动任务</h2><p>开始任务后，cyClaw 会编译历史决策、失败方案和相关文档。</p></div>
  <button class="action primary" data-command="cyclaw.beginTask">开始任务</button>
</section>`;
  const reconciliation = snapshot.reconciliation;
  const reconciliationNotice = reconciliation && reconciliation.findings.length > 0 ? `
<div class="memory-alert"><strong>最近知识对账</strong><span>重复 ${reconciliation.duplicate_count} · 冲突 ${reconciliation.conflict_count} · 失效 ${reconciliation.stale_count}</span></div>` : "";
  const events = snapshot.events.slice(0, 6).map((event) => `
    <li><span class="event-mark ${escapeHtml(event.event_type)}"></span><span class="event-text">${escapeHtml(event.summary)}</span><time>${new Date(event.created_at).toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" })}</time></li>`).join("");
  const error = snapshot.error ? `<div class="notice error"><strong>cyClaw 暂不可用</strong><span>${escapeHtml(snapshot.error)}</span></div>` : "";
  return `<!doctype html>
<html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';">
<style>
:root{color-scheme:light dark}*{box-sizing:border-box}body{margin:0;padding:0 0 18px;color:var(--vscode-foreground);background:var(--vscode-sideBar-background);font-family:var(--vscode-font-family);font-size:13px;line-height:1.45;letter-spacing:0}.shell{min-width:0}.brand{padding:18px 16px 14px;border-bottom:1px solid var(--vscode-sideBarSectionHeader-border,var(--vscode-panel-border));background:var(--vscode-sideBarSectionHeader-background)}.brand-row{display:flex;align-items:center;justify-content:space-between;gap:10px}.brand h1{font-size:20px;line-height:1.2;margin:0;font-weight:650}.brand .version{font-size:11px;color:var(--vscode-descriptionForeground)}.brand p{margin:7px 0 0;color:var(--vscode-descriptionForeground);font-size:12px}.task-band{padding:14px;border-bottom:1px solid var(--vscode-panel-border);background:var(--vscode-textBlockQuote-background)}.task-heading{display:flex;align-items:flex-start;justify-content:space-between;gap:10px}.task-heading span,.empty-task span{font-size:11px;color:var(--vscode-descriptionForeground)}.task-band h2{font-size:15px;margin:2px 0 0}.task-band p{margin:7px 0;color:var(--vscode-descriptionForeground);font-size:12px}.task-metrics{display:grid;grid-template-columns:1fr 1fr;gap:5px 10px;padding:8px 0;border-top:1px solid var(--vscode-panel-border);border-bottom:1px solid var(--vscode-panel-border);font-size:11px}.task-band h3{font-size:11px;margin:10px 0 4px;color:var(--vscode-descriptionForeground)}.context-facts{list-style:none;padding:0;margin:0}.context-facts li{padding:5px 0;border-bottom:1px solid var(--vscode-panel-border)}.context-facts strong,.context-facts small{display:block}.context-facts strong{font-size:12px}.context-facts small{margin-top:2px;color:var(--vscode-descriptionForeground);font-size:10px}.task-actions{display:grid;grid-template-columns:1fr 1fr 1fr;gap:5px;margin-top:10px}.empty-task{display:flex;align-items:center;justify-content:space-between;gap:10px}.empty-task>div{min-width:0}.empty.compact{padding:8px 0}.memory-alert{display:flex;justify-content:space-between;gap:10px;padding:8px 14px;border-bottom:1px solid var(--vscode-panel-border);color:var(--vscode-editorWarning-foreground);background:var(--vscode-inputValidation-warningBackground);font-size:11px}.tag.active{color:var(--vscode-testing-iconPassed);background:var(--vscode-diffEditor-insertedTextBackground)}.status-strip{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));border-bottom:1px solid var(--vscode-panel-border)}.stat{padding:12px 10px;border-right:1px solid var(--vscode-panel-border);min-width:0}.stat:last-child{border-right:0}.stat strong{display:block;font-size:18px;line-height:1.1}.stat span{display:block;margin-top:5px;color:var(--vscode-descriptionForeground);font-size:11px;overflow-wrap:anywhere}.section{padding:15px 14px 0}.section-head{display:flex;align-items:center;justify-content:space-between;gap:8px;margin-bottom:8px}.section h2{font-size:12px;text-transform:uppercase;margin:0;color:var(--vscode-descriptionForeground);font-weight:650}.state{display:flex;align-items:center;gap:7px;font-size:12px}.dot{width:8px;height:8px;border-radius:50%;background:var(--vscode-testing-iconPassed)}.dot.off{background:var(--vscode-descriptionForeground)}.mode-band{display:flex;align-items:center;justify-content:space-between;gap:10px;padding:10px 12px;border-left:3px solid var(--vscode-focusBorder);background:var(--vscode-textBlockQuote-background)}.mode-band strong{font-size:14px}.mode-band span{font-size:11px;color:var(--vscode-descriptionForeground)}.actions{display:grid;grid-template-columns:1fr 1fr;gap:7px}.action{min-height:34px;border:1px solid var(--vscode-button-border,transparent);border-radius:4px;padding:7px 9px;background:var(--vscode-button-secondaryBackground);color:var(--vscode-button-secondaryForeground);font:inherit;text-align:left;cursor:pointer}.action:hover{background:var(--vscode-button-secondaryHoverBackground)}.action.primary{background:var(--vscode-button-background);color:var(--vscode-button-foreground)}.action.primary:hover{background:var(--vscode-button-hoverBackground)}.work-list{display:flex;flex-direction:column;border-top:1px solid var(--vscode-panel-border)}.work-item{display:flex;align-items:center;justify-content:space-between;gap:10px;width:100%;padding:10px 2px;border:0;border-bottom:1px solid var(--vscode-panel-border);background:transparent;color:var(--vscode-foreground);font:inherit;text-align:left;cursor:pointer}.work-item:hover{background:var(--vscode-list-hoverBackground)}.work-main{min-width:0}.work-main strong,.work-main small{display:block;overflow:hidden;text-overflow:ellipsis}.work-main strong{white-space:normal;font-weight:550}.work-main small{margin-top:3px;color:var(--vscode-descriptionForeground);white-space:nowrap}.score,.tag{flex:0 0 auto;border-radius:3px;padding:2px 5px;font-size:11px}.score.high{color:var(--vscode-testing-iconPassed);background:var(--vscode-diffEditor-insertedTextBackground)}.score.medium,.tag{color:var(--vscode-editorWarning-foreground);background:var(--vscode-diffEditor-unchangedRegionBackground)}.score.low{color:var(--vscode-errorForeground);background:var(--vscode-diffEditor-removedTextBackground)}.events{list-style:none;margin:0;padding:0}.events li{display:grid;grid-template-columns:8px minmax(0,1fr) auto;align-items:center;gap:8px;padding:7px 0}.event-mark{width:6px;height:6px;border-radius:50%;background:var(--vscode-focusBorder)}.event-text{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.events time{color:var(--vscode-descriptionForeground);font-size:11px}.empty{padding:14px 0;color:var(--vscode-descriptionForeground);font-size:12px}.notice{margin:12px 14px 0;padding:10px 12px;border-left:3px solid var(--vscode-editorWarning-foreground);background:var(--vscode-inputValidation-warningBackground)}.notice strong,.notice span{display:block}.notice span{margin-top:4px;font-size:12px}.notice.error{border-left-color:var(--vscode-errorForeground);background:var(--vscode-inputValidation-errorBackground)}@media(max-width:380px){.status-strip{grid-template-columns:repeat(2,minmax(0,1fr))}.stat{border-bottom:1px solid var(--vscode-panel-border)}.actions,.task-actions{grid-template-columns:1fr}.mode-band,.empty-task{align-items:flex-start;flex-direction:column}}
</style></head><body><main class="shell">
<header class="brand"><div class="brand-row"><h1>cyClaw</h1><span class="version">v${escapeHtml(String(context.extension.packageJSON.version ?? "0.1.0"))}</span></div><p>Coding Agent 的项目记忆与上下文控制层</p></header>
${error}
${taskSection}
${reconciliationNotice}
<div class="status-strip"><div class="stat"><strong>${snapshot.candidates.length}</strong><span>待处理候选</span></div><div class="stat"><strong>${snapshot.patches.length}</strong><span>文档草稿</span></div><div class="stat"><strong>${status?.fact_patch_pending ?? 0}</strong><span>事实草稿</span></div><div class="stat"><strong>${status?.git_has_changes ? "有" : "无"}</strong><span>Git 变化</span></div></div>
<section class="section"><div class="section-head"><h2>运行状态</h2><div class="state"><span class="dot ${watching ? "" : "off"}"></span>${watching ? "事件监听中" : "监听已停止"}</div></div><div class="mode-band"><div><strong>${escapeHtml(mode)}</strong><br><span>${escapeHtml(snapshot.models?.active_provider ? `模型：${snapshot.models.active_provider}` : "本地规则")}</span></div><button class="action" data-command="cyclaw.configureMode">选择策略</button></div></section>
<section class="section"><div class="section-head"><h2>快捷操作</h2></div><div class="actions"><button class="action primary" data-command="cyclaw.runAgent">运行 Agent</button><button class="action" data-command="cyclaw.watchOnce">检查变化</button><button class="action" data-command="cyclaw.batchCandidates">批量处理</button><button class="action" data-command="cyclaw.configureModel">配置模型</button><button class="action" data-command="cyclaw.configurePermissions">高级权限</button><button class="action" data-command="cyclaw.showOutput">运行日志</button></div></section>
<section class="section"><div class="section-head"><h2>待处理</h2><span>${snapshot.candidates.length + snapshot.patches.length + (status?.fact_patch_pending ?? 0)} 项</span></div><div class="work-list">${candidates}${patches}${factPatches}${!candidates && !patches && !factPatches ? '<div class="empty">当前知识状态已同步，没有待处理内容。</div>' : ""}</div></section>
<section class="section"><div class="section-head"><h2>最近活动</h2><span>${snapshot.events.length} 条</span></div>${events ? `<ul class="events">${events}</ul>` : '<div class="empty">等待第一次项目变化。</div>'}</section>
</main><script nonce="${nonce}">const vscode=acquireVsCodeApi();document.addEventListener('click',(event)=>{const target=event.target.closest('button');if(!target)return;if(target.dataset.command)vscode.postMessage({command:target.dataset.command});if(target.dataset.kind==='candidate')vscode.postMessage({command:'openCandidate',id:target.dataset.id});if(target.dataset.kind==='patch')vscode.postMessage({command:'openPatch',id:target.dataset.id});if(target.dataset.kind==='factPatch')vscode.postMessage({command:'openFactPatch',id:target.dataset.id});});</script></body></html>`;
}

function activityIcon(eventType: string): string {
  switch (eventType) {
    case "git_changed": return "git-compare";
    case "knowledge_candidate_created": return "lightbulb";
    case "document_patch_created": return "diff";
    case "document_patch_applied": return "check";
    case "model_called": return "sparkle";
    case "policy_changed": return "shield";
    default: return "history";
  }
}

function renderCandidateHtml(candidate: KnowledgeCandidate): string {
  const reasons = candidate.reasons.map((reason) => `<li>${escapeHtml(reason)}</li>`).join("");
  const files = candidate.related_files.map((file) => `<li><code>${escapeHtml(file)}</code></li>`).join("");
  return `<!doctype html>
<html lang="zh-CN"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<style>
body{font-family:var(--vscode-font-family);color:var(--vscode-foreground);padding:24px;max-width:860px;margin:auto;line-height:1.6}
h1{font-size:22px;margin:0 0 8px}h2{font-size:15px;margin-top:24px;border-bottom:1px solid var(--vscode-panel-border);padding-bottom:8px}
.meta{color:var(--vscode-descriptionForeground);display:flex;gap:16px;flex-wrap:wrap}.target{padding:12px;border-left:3px solid var(--vscode-focusBorder);background:var(--vscode-textBlockQuote-background)}
.actions{display:flex;gap:8px;margin-top:28px}button{border:0;padding:8px 14px;color:var(--vscode-button-foreground);background:var(--vscode-button-background);cursor:pointer}button.secondary{background:var(--vscode-button-secondaryBackground);color:var(--vscode-button-secondaryForeground)}
code{font-family:var(--vscode-editor-font-family)}
</style></head><body>
<h1>${escapeHtml(candidate.summary)}</h1>
<div class="meta"><span>重要性：${escapeHtml(candidate.importance)}</span><span>置信度：${candidate.confidence}%</span><span>${candidate.reviewed_by_model ? "模型已审查" : "本地规则判断"}</span></div>
${candidate.model_rationale ? `<h2>模型结论</h2><div class="target"><strong>${escapeHtml(candidate.model_recommendation ?? "keep")}</strong><br>${escapeHtml(candidate.model_rationale)}</div>` : ""}
<h2>建议更新</h2><div class="target"><code>${escapeHtml(candidate.recommended_doc)}</code></div>
<h2>判断依据</h2><ul>${reasons}</ul>
<h2>关联文件</h2><ul>${files}</ul>
<div class="actions"><button onclick="send('draft')">接受并生成草稿</button><button class="secondary" onclick="send('source')">打开来源</button><button class="secondary" onclick="send('ignore')">忽略</button></div>
<script>const vscode=acquireVsCodeApi();function send(command){vscode.postMessage({command})}</script>
</body></html>`;
}

function factPatchStatusOrder(status: FactPatch["status"]): number {
  if (status === "applying" || status === "reverting") return 0;
  return status === "pending" ? 1 : status === "applied" ? 2 : 3;
}

function factPatchStatusLabel(status: FactPatch["status"]): string {
  if (status === "applying") return "应用恢复中";
  if (status === "reverting") return "撤销恢复中";
  return status === "pending" ? "待应用" : status === "applied" ? "已应用，可撤销" : "已撤销";
}

function knowledgeOperationLabel(operation: DocumentPatch["operation"] | undefined): string {
  switch (operation) {
    case "update": return "更新";
    case "merge": return "合并";
    case "supersede": return "取代";
    case "delete": return "删除";
    case "create":
    default:
      return "新增";
  }
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[character] ?? character));
}
