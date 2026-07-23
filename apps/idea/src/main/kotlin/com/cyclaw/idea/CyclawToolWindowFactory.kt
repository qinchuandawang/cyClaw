package com.cyclaw.idea

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.service
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.content.ContentFactory
import java.awt.BorderLayout
import java.awt.FlowLayout
import javax.swing.JButton
import javax.swing.JComboBox
import javax.swing.JPanel
import javax.swing.JTextArea

class CyclawToolWindowFactory : ToolWindowFactory {
    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = CyclawToolWindowPanel(project)
        val content = ContentFactory.getInstance().createContent(panel, "项目知识", false)
        toolWindow.contentManager.addContent(content)
    }
}

class CyclawToolWindowPanel(private val project: Project) : JPanel(BorderLayout()) {
    private val service = project.service<CyclawProjectService>()
    private val output = JTextArea()
    private val candidates = JComboBox<KnowledgeCandidate>()
    private val patches = JComboBox<DocumentPatch>()

    init {
        output.isEditable = false
        output.lineWrap = true
        output.wrapStyleWord = true

        add(toolbar(), BorderLayout.NORTH)
        add(JBScrollPane(output), BorderLayout.CENTER)
        refresh()
    }

    private fun toolbar(): JPanel {
        val panel = JPanel(FlowLayout(FlowLayout.LEFT))
        panel.add(button("刷新") { refresh() })
        panel.add(button("初始化") { runAction("初始化") { service.initProject() } })
        panel.add(button("扫描") { runAction("扫描") { service.scanProject() } })
        panel.add(button("检查一次") { runAction("检查一次") { service.watchOnce() } })
        panel.add(button("启动监听") { service.startWatch(::appendOutput) })
        panel.add(button("停止监听") { service.stopWatch() })
        panel.add(candidates)
        panel.add(button("接受") { selectedCandidate()?.let { runAction("接受候选") { service.acceptCandidate(it.id) } } })
        panel.add(button("忽略") { selectedCandidate()?.let { runAction("忽略候选") { service.ignoreCandidate(it.id) } } })
        panel.add(patches)
        panel.add(button("应用草稿") { applySelectedPatch() })
        return panel
    }

    private fun refresh() {
        ApplicationManager.getApplication().executeOnPooledThread {
            val snapshot = service.snapshot()
            ApplicationManager.getApplication().invokeLater {
                render(snapshot)
            }
        }
    }

    private fun render(snapshot: CyclawSnapshot) {
        candidates.removeAllItems()
        patches.removeAllItems()

        if (snapshot.error != null) {
            output.text = "cyClaw 读取失败：${snapshot.error}"
            return
        }

        snapshot.candidates.forEach(candidates::addItem)
        snapshot.patches.forEach(patches::addItem)

        val status = snapshot.status
        output.text = buildString {
            appendLine("cyClaw 项目知识状态")
            appendLine()
            appendLine("已初始化: ${yesNo(status?.initialized == true)}")
            appendLine("Git 未提交变更: ${yesNo(status?.gitHasChanges == true)}")
            appendLine("待处理候选知识: ${status?.inboxPending ?: 0}")
            appendLine("待应用文档草稿: ${status?.draftPending ?: 0}")
            appendLine("本地索引: ${yesNo(status?.indexExists == true)}")
            appendLine()
            appendLine("候选知识")
            snapshot.candidates.forEach {
                appendLine("- ${it.summary} -> ${it.recommendedDoc}")
            }
            appendLine()
            appendLine("文档草稿")
            snapshot.patches.forEach {
                appendLine("- ${it.summary} -> ${it.targetDoc}")
            }
            appendLine()
            appendLine("建议下一步")
            status?.suggestedNextSteps.orEmpty().forEach {
                appendLine("- $it")
            }
        }
    }

    private fun runAction(name: String, action: () -> CyclawCommandResult) {
        ApplicationManager.getApplication().executeOnPooledThread {
            val result = action()
            ApplicationManager.getApplication().invokeLater {
                appendOutput("\n[$name]\n")
                appendOutput(result.stdout)
                if (result.stderr.isNotBlank()) {
                    appendOutput(result.stderr)
                }
                refresh()
            }
        }
    }

    private fun applySelectedPatch() {
        val patch = selectedPatch() ?: return
        val confirmed = Messages.showYesNoDialog(
            project,
            "确认应用文档草稿 ${patch.id}？",
            "应用 cyClaw 文档草稿",
            Messages.getQuestionIcon(),
        )
        if (confirmed == Messages.YES) {
            runAction("应用草稿") { service.applyPatch(patch.id) }
        }
    }

    private fun selectedCandidate(): KnowledgeCandidate? = candidates.selectedItem as? KnowledgeCandidate

    private fun selectedPatch(): DocumentPatch? = patches.selectedItem as? DocumentPatch

    private fun appendOutput(text: String) {
        output.append(text)
    }

    private fun button(text: String, action: () -> Unit): JButton =
        JButton(text).apply {
            addActionListener { action() }
        }

    private fun yesNo(value: Boolean): String = if (value) "是" else "否"
}
