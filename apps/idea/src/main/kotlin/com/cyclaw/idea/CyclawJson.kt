package com.cyclaw.idea

import com.google.gson.JsonElement
import com.google.gson.JsonObject
import com.google.gson.JsonParser

object CyclawJson {
    fun parseObject(text: String): JsonObject = JsonParser.parseString(text).asJsonObject

    fun projectStatus(value: JsonObject): ProjectStatus =
        ProjectStatus(
            initialized = value.bool("initialized"),
            gitHasChanges = value.bool("git_has_changes"),
            inboxPending = value.int("inbox_pending"),
            draftPending = value.int("draft_pending"),
            indexExists = value.bool("index_exists"),
            suggestedNextSteps = value.arrayStrings("suggested_next_steps"),
        )

    fun candidates(value: JsonObject): List<KnowledgeCandidate> =
        value.arrayObjects("candidates").map {
            KnowledgeCandidate(
                id = it.string("id"),
                summary = it.string("summary"),
                importance = it.string("importance"),
                recommendedDoc = it.string("recommended_doc"),
                relatedFiles = it.arrayStrings("related_files"),
            )
        }

    fun patches(value: JsonObject): List<DocumentPatch> =
        value.arrayObjects("patches").map {
            DocumentPatch(
                id = it.string("id"),
                summary = it.string("summary"),
                targetDoc = it.string("target_doc"),
                candidateId = it.string("candidate_id"),
                operation = it.string("operation").ifBlank { "create" },
            )
        }

    private fun JsonObject.string(name: String): String =
        get(name)?.takeUnless(JsonElement::isJsonNull)?.asString.orEmpty()

    private fun JsonObject.bool(name: String): Boolean =
        get(name)?.takeUnless(JsonElement::isJsonNull)?.asBoolean ?: false

    private fun JsonObject.int(name: String): Int =
        get(name)?.takeUnless(JsonElement::isJsonNull)?.asInt ?: 0

    private fun JsonObject.arrayStrings(name: String): List<String> =
        getAsJsonArray(name)?.mapNotNull { element ->
            element.takeUnless(JsonElement::isJsonNull)?.asString
        }.orEmpty()

    private fun JsonObject.arrayObjects(name: String): List<JsonObject> =
        getAsJsonArray(name)?.mapNotNull { element ->
            element.takeUnless(JsonElement::isJsonNull)?.asJsonObject
        }.orEmpty()
}
