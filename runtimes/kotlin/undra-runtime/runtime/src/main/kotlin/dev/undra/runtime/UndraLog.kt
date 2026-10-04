package dev.undra.runtime

import java.util.logging.Level
import java.util.logging.Logger

/** The runtime's own diagnostics, through `java.util.logging` (Android routes it to logcat). */
internal object UndraLog {
    private val logger: Logger = Logger.getLogger("dev.undra.runtime")

    fun warn(message: String, cause: Throwable? = null) {
        logger.log(Level.WARNING, message, cause)
    }

    /** A failure nobody could see (ADR-032, amendment A): `SEVERE`, which Android's logcat shows as an error. */
    fun error(message: String, cause: Throwable? = null) {
        logger.log(Level.SEVERE, message, cause)
    }

    fun debug(message: String) {
        logger.log(Level.FINE, message)
    }
}

/** Where the runtime is running. */
internal object Platform {
    /** `true` when Android's `android.os.Looper` is loadable. */
    val isAndroid: Boolean = try {
        Class.forName("android.os.Looper")
        true
    } catch (e: ClassNotFoundException) {
        false
    } catch (e: LinkageError) {
        false
    }

    /** The `platform` string of `RuntimeConfig` and `Hello`. */
    val name: String get() = if (isAndroid) "android" else "jvm"
}

/** The version this runtime reports in `Hello`. */
internal const val UNDRA_RUNTIME_VERSION: String = "1.0.0-rc.1"
