package com.cyclaw.idea

data class ProjectStatus(
    val initialized: Boolean = false,
    val gitHasChanges: Boolean = false,
    val inboxPending: Int = 0,
    val draftPending: Int = 0,
    val indexExists: Boolean = false,
    val suggestedNextSteps: List<String> = emptyList(),
)

data class KnowledgeCandidate(
    val id: String,
    val summary: String,
    val importance: String,
    val recommendedDoc: String,
    val relatedFiles: List<String>,
) {
    override fun toString(): String = "$summary -> $recommendedDoc"
}

data class DocumentPatch(
    val id: String,
    val summary: String,
    val targetDoc: String,
    val candidateId: String,
    val operation: String,
) {
    override fun toString(): String = "${operationLabel(operation)} · $summary -> $targetDoc"
}

private fun operationLabel(operation: String): String =
    when (operation) {
        "update" -> "更新"
        "merge" -> "合并"
        "supersede" -> "取代"
        "delete" -> "删除"
        else -> "新增"
    }

data class CyclawSnapshot(
    val status: ProjectStatus? = null,
    val candidates: List<KnowledgeCandidate> = emptyList(),
    val patches: List<DocumentPatch> = emptyList(),
    val error: String? = null,
)

data class CyclawCommandResult(
    val exitCode: Int,
    val stdout: String,
    val stderr: String,
)
