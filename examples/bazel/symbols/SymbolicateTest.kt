// A crash report of a Bazel-built core, resolved with the symbol files of that build (ADR-061, ADR-046).
//
// The core is `//:core_release_host`, the release build `undra_core` makes (stripped, as it ships); the symbol files are the `symbols`
// output group of the same target. The test makes the core panic through `crashForSymbolsTest`, takes the `UndraPanicReport` the
// app's `onPanic` receives, writes it as JSON and runs `undra symbolicate` on it. Every check prints one `bazel symbols:` line; any
// failure exits 1, which is what makes `bazel test //symbols:symbolicate_test` fail.
package dev.undra.bazel.hello.symbols

import dev.undra.bazel.hello.UndraHelloCore
import dev.undra.bazel.hello.crashForSymbolsTest
import dev.undra.bazel.hello.greeting
import dev.undra.runtime.LoadOptions
import dev.undra.runtime.UndraCallError
import dev.undra.runtime.adapters.UndraPanicReport
import java.io.File
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit
import kotlin.system.exitProcess

private var failed = false

private fun check(what: String, ok: Boolean) {
    println("bazel symbols: ${if (ok) "ok  " else "FAIL"} $what")
    if (!ok) failed = true
}

/** A required system property, as a file: Bazel's `$(rootpath ..)` of an input, relative to the runfiles this test runs in. */
private fun input(name: String): File = File(requireNotNull(System.getProperty(name)) { "no $name" }).absoluteFile

/** The text of a JSON string literal. */
private fun json(text: String): String = buildString {
    append('"')
    for (c in text) {
        when {
            c == '"' -> append("\\\"")
            c == '\\' -> append("\\\\")
            c < ' ' -> append("\\u%04x".format(c.code))
            else -> append(c)
        }
    }
    append('"')
}

/** The report as the JSON `undra symbolicate` reads: what an app's crash reporter keeps of an `onPanic` (production.html#symbols). */
private fun reportJson(report: UndraPanicReport): String =
    """
    {
      "message": ${json(report.message)},
      "location": ${json(report.location)},
      "operation": ${json(report.operation)},
      "thread": ${json(report.thread)},
      "namespace": ${json(report.namespace)},
      "coreVersion": ${json(report.coreVersion)},
      "schemaHash": ${json("0x%016x".format(report.schemaHash))},
      "imageId": ${json(report.imageId)},
      "frames": [${report.frames.joinToString(", ") { """{ "address": ${it.address} }""" }}]
    }
    """.trimIndent()

/** The directory that holds a tool: on `PATH`, else where a Linux distribution puts LLVM's. */
private fun toolDirectory(vararg names: String): File? {
    val path = System.getenv("PATH").orEmpty().split(File.pathSeparator).filter { it.isNotEmpty() }
    val llvm = File("/usr/lib").listFiles { f -> f.name.startsWith("llvm-") }.orEmpty().map { File(it, "bin").path }
    return (path + llvm).map(::File).firstOrNull { dir -> names.any { File(dir, it).canExecute() } }
}

/** The 1-based line of the `panic!` in `crash_for_symbols_test`, from the source the core was built from. */
private fun panicLine(source: File): Int {
    val lines = source.readLines()
    val function = lines.indexOfFirst { it.contains("pub fn crash_for_symbols_test(") }
    require(function >= 0) { "no crash_for_symbols_test in ${source.path}" }
    val panic = lines.drop(function).indexOfFirst { it.contains("panic!(") }
    require(panic >= 0) { "no panic! after it" }
    return function + panic + 1
}

fun main() {
    val library = input("undra.test.library")
    val symbols = input("undra.test.symbols")
    val cli = input("undra.test.cli")
    val line = panicLine(input("undra.test.source"))
    check("the release core, the symbol files, the CLI and the source are inputs: ${library.name}", library.isFile && symbols.isDirectory && cli.isFile)
    System.setProperty("undra.native.${UndraHelloCore.NAMESPACE}.path", library.path)

    // The symbol files are laid out as `undra build --release` writes them: symbols/ and, beside it, the host library's.
    val manifestFile = File(symbols, "symbols/manifest.json")
    check("the symbols group has symbols/manifest.json", manifestFile.isFile)
    val twin = File(symbols, "host").listFiles().orEmpty().filter { it.name.endsWith(".dSYM") || it.name.endsWith(".debug") }
    check("the symbols group has the host library's symbols: ${twin.map { it.name }}", twin.size == 1)
    val manifest = manifestFile.readText()

    // 1. The core panics, as an app's would, and the app's `onPanic` receives the report.
    val received = CompletableFuture<UndraPanicReport>()
    val core = UndraHelloCore.load(LoadOptions(onPanic = { report -> received.complete(report) }))
    val outcome = try {
        crashForSymbolsTest("kaboom")
        null
    } catch (e: UndraCallError.Panicked) {
        e
    }
    check("the call that panicked fails with a typed error: ${outcome?.panicMessage}", outcome != null)
    val report = received.get(60, TimeUnit.SECONDS) // a failure's bound, not a delay: the handler completes it
    val after = greeting("symbols")
    check("the core lives on: the next call answers \"$after\"", after == "Hello, symbols, from the bazel-hello core")
    core.close()
    println("bazel symbols: report: ${report.summary}, ${report.frames.size} frames, image ${report.imageId}")
    check("the report is of this core: ${report.namespace} ${report.coreVersion}", report.namespace == "hello_core" && report.coreVersion.isNotEmpty())
    check("the panic location names the core's source and its line: ${report.location}", report.location.contains("core/src/lib.rs:$line:"))
    check("a release build reports addresses, not names", report.frames.isNotEmpty() && report.frames.all { it.symbol == null && it.line == null })
    // The manifest keys the symbol file by the library's identity (Mach-O UUID, ELF build id), which is what a report carries.
    val listed = Regex("\"imageId\"\\s*:\\s*\"([0-9a-f]*)\"").find(manifest)?.groupValues?.get(1).orEmpty()
    check("the report's image is the one the manifest lists: '${report.imageId}' and '$listed'", listed.isNotEmpty() && listed == report.imageId)

    // 2. `undra symbolicate` resolves the addresses with the symbol files of the same build.
    val work = File(System.getenv("TEST_TMPDIR") ?: System.getProperty("java.io.tmpdir"), "symbolicate").apply { mkdirs() }
    val reportFile = File(work, "report.json").apply { writeText(reportJson(report)) }
    val builder = ProcessBuilder(cli.path, "symbolicate", "--symbols", File(symbols, "symbols").path, reportFile.path)
        .directory(work)
        .redirectErrorStream(true)
    val environment = builder.environment()
    // The tool that reads the symbol files is the machine's: atos on macOS, llvm-symbolizer (or llvm-addr2line) on Linux.
    val macos = System.getProperty("os.name").startsWith("Mac")
    if (!macos) {
        val tools = toolDirectory("llvm-symbolizer", "llvm-addr2line")
        check("llvm-symbolizer or llvm-addr2line is installed (apt install llvm)", tools != null)
        if (tools != null) environment["PATH"] = environment["PATH"].orEmpty() + File.pathSeparator + tools.path
    }
    val process = builder.start()
    val output = process.inputStream.bufferedReader().readText()
    val status = process.waitFor()
    println(output)
    check("undra symbolicate exits 0 (it exited $status)", status == 0)

    val frames = output.lines().filter { it.contains("hello_core::crash_for_symbols_test") }
    val resolved = frames.firstOrNull { it.contains("core/src/lib.rs:$line)") }
    check("a frame is hello_core::crash_for_symbols_test at core/src/lib.rs:$line: $resolved", resolved != null)
    // A release build names the project `/undra/app` (ADR-052): the path is the same in any checkout and names no machine.
    check("the frame's path is remapped to /undra/app: $resolved", resolved != null && resolved.contains("(/undra/app/core/src/lib.rs:"))

    println("bazel symbols: ${if (failed) "FAILED" else "passed"}")
    if (failed) exitProcess(1)
}
