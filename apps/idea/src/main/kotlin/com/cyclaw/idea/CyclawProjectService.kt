package com.cyclaw.idea

import com.google.gson.JsonObject
import com.google.gson.JsonParser
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.OSProcessHandler
import com.intellij.execution.util.ExecUtil
import com.intellij.openapi.Disposable
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.components.Service
import com.intellij.openapi.project.Project
import java.nio.file.Files
import java.nio.file.Path
import java.nio.charset.StandardCharsets
import java.util.concurrent.TimeUnit

@Service(Service.Level.PROJECT)
class CyclawProjectService(private val project: Project) : Disposable {
    private var watchHandler: OSProcessHandler? = null

    fun snapshot(): CyclawSnapshot =
        try {
            val status = CyclawJson.projectStatus(callMcpTool("get_project_status"))
            val candidates = CyclawJson.candidates(callMcpTool("list_pending_knowledge"))
            val patches = CyclawJson.patches(callMcpTool("list_document_patches"))
            CyclawSnapshot(status = status, candidates = candidates, patches = patches)
        } catch (error: Exception) {
            CyclawSnapshot(error = error.message ?: error.toString())
        }

    fun initProject(): CyclawCommandResult = runCyclaw("init")

    fun scanProject(): CyclawCommandResult = runCyclaw("scan")

    fun watchOnce(): CyclawCommandResult = runCyclaw("watch", "--once")

    fun acceptCandidate(id: String): CyclawCommandResult = runCyclaw("inbox", "accept", id)

    fun ignoreCandidate(id: String): CyclawCommandResult = runCyclaw("inbox", "ignore", id)

    fun applyPatch(id: String): CyclawCommandResult = runCyclaw("draft", "apply", id)

    fun startWatch(onText: (String) -> Unit) {
        if (watchHandler != null) {
            onText("cyClaw watch 已在运行。\n")
            return
        }

        val commandLine = commandLine(listOf("watch", "--debounce-ms", "600"))
        val handler = OSProcessHandler(commandLine)
        watchHandler = handler
        handler.addProcessListener(CyclawProcessListener({ text ->
            ApplicationManager.getApplication().invokeLater {
                onText(text)
            }
        }) {
            watchHandler = null
        })
        handler.startNotify()
        onText("cyClaw watch 已启动。\n")
    }

    fun stopWatch() {
        watchHandler?.destroyProcess()
        watchHandler = null
    }

    override fun dispose() {
        stopWatch()
    }

    private fun callMcpTool(toolName: String): JsonObject {
        val initialize = """{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"""
        val call = """{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"$toolName","arguments":{}}}"""
        val input = frame(initialize) + frame(call)
        val output = runCyclawWithInput(listOf("mcp"), input)

        if (output.exitCode != 0) {
            error(output.stderr.ifBlank { "MCP 工具调用失败: $toolName" })
        }

        val response = parseFrames(output.stdout).firstOrNull { it.get("id")?.asInt == 2 }
            ?: error("MCP 工具没有返回响应: $toolName")
        response.getAsJsonObject("error")?.let {
            error(it.get("message")?.asString ?: "MCP 工具调用失败: $toolName")
        }

        val text = response
            .getAsJsonObject("result")
            .getAsJsonArray("content")
            .get(0)
            .asJsonObject
            .get("text")
            .asString

        return CyclawJson.parseObject(text)
    }

    private fun runCyclaw(vararg args: String): CyclawCommandResult {
        val output = ExecUtil.execAndGetOutput(commandLine(args.toList()))
        return CyclawCommandResult(
            exitCode = output.exitCode,
            stdout = output.stdout,
            stderr = output.stderr,
        )
    }

    private fun runCyclawWithInput(args: List<String>, input: String): CyclawCommandResult {
        val process = commandLine(args).createProcess()
        process.outputStream.use { stream ->
            stream.write(input.toByteArray(StandardCharsets.UTF_8))
        }
        if (!process.waitFor(30, TimeUnit.SECONDS)) {
            process.destroyForcibly()
            error("cyClaw MCP 调用超时")
        }
        return CyclawCommandResult(
            exitCode = process.exitValue(),
            stdout = String(process.inputStream.readAllBytes(), StandardCharsets.UTF_8),
            stderr = String(process.errorStream.readAllBytes(), StandardCharsets.UTF_8),
        )
    }

    private fun commandLine(args: List<String>): GeneralCommandLine {
        val root = project.basePath ?: error("项目没有 basePath")
        val isCyclawRepository = Files.exists(Path.of(root, "crates", "cyclaw-cli", "Cargo.toml"))
        val command = if (isCyclawRepository) "cargo" else "cyclaw"
        val commandArgs = if (isCyclawRepository) {
            mutableListOf("run", "-p", "cyclaw-cli", "--").apply {
                addAll(args)
                add("--path")
                add(root)
            }
        } else {
            args.toMutableList().apply {
                add("--path")
                add(root)
            }
        }

        return GeneralCommandLine(command)
            .withParameters(commandArgs)
            .withWorkDirectory(root)
            .withCharset(StandardCharsets.UTF_8)
    }

    private fun frame(body: String): String {
        val length = body.toByteArray(StandardCharsets.UTF_8).size
        return "Content-Length: $length\r\n\r\n$body"
    }

    private fun parseFrames(output: String): List<JsonObject> {
        val messages = mutableListOf<JsonObject>()
        var cursor = 0

        while (cursor < output.length) {
            val headerEnd = output.indexOf("\r\n\r\n", cursor)
            if (headerEnd < 0) break

            val header = output.substring(cursor, headerEnd)
            val length = Regex("Content-Length:\\s*(\\d+)", RegexOption.IGNORE_CASE)
                .find(header)
                ?.groupValues
                ?.get(1)
                ?.toIntOrNull()
                ?: break
            val bodyStart = headerEnd + 4
            val body = output.substring(bodyStart, bodyStart + length)
            messages.add(JsonParser.parseString(body).asJsonObject)
            cursor = bodyStart + length
        }

        return messages
    }
}
