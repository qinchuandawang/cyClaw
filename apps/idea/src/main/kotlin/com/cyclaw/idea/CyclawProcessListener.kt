package com.cyclaw.idea

import com.intellij.execution.process.ProcessEvent
import com.intellij.execution.process.ProcessListener
import com.intellij.openapi.util.Key

class CyclawProcessListener(
    private val onText: (String) -> Unit,
    private val onTerminated: () -> Unit,
) : ProcessListener {
    override fun onTextAvailable(event: ProcessEvent, outputType: Key<*>) {
        onText(event.text)
    }

    override fun processTerminated(event: ProcessEvent) {
        onText("cyClaw watch 已退出，退出码: ${event.exitCode}\n")
        onTerminated()
    }
}
