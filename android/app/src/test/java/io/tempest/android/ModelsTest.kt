package io.tempest.android

import io.tempest.android.core.AppStatus
import io.tempest.android.core.BridgeEvent
import io.tempest.android.core.ComponentStatus
import io.tempest.android.core.DiagnosticsReport
import io.tempest.android.core.Envelope
import io.tempest.android.core.GameCatalogue
import io.tempest.android.core.InstallPhase
import io.tempest.android.core.InstallProgress
import io.tempest.android.core.ParsedLink
import io.tempest.android.core.SessionSnapshot
import io.tempest.android.core.TempestConfig
import io.tempest.android.core.TempestException
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * These pin the wire format between Rust and Kotlin.
 *
 * The two sides are compiled separately, so a rename on the Rust side would
 * otherwise only show up as a mysteriously empty screen on a device. Each
 * fixture below is the literal JSON the core emits.
 */
class ModelsTest {

    private val json = Json { ignoreUnknownKeys = true; isLenient = true }

    @Test
    fun `success envelope carries its payload`() {
        val envelope = json.decodeFromString<Envelope>("""{"ok":true,"data":{"a":1}}""")
        assertTrue(envelope.ok)
        assertNotNull(envelope.data)
        assertNull(envelope.error)
    }

    @Test
    fun `error envelope carries the machine-readable kind`() {
        val envelope = json.decodeFromString<Envelope>(
            """{"ok":false,"error":"your Vortex session has expired","kind":"auth"}""",
        )
        assertFalse(envelope.ok)
        assertEquals("auth", envelope.kind)
        assertEquals("your Vortex session has expired", envelope.error)
    }

    @Test
    fun `app status decodes with the snake_case names Rust emits`() {
        val status = json.decodeFromString<AppStatus>(
            """
            {
              "version":"0.2.0",
              "signed_in":true,
              "username":"someone",
              "runtime_ready":false,
              "missing_components":["rootfs","hangover"],
              "session":{"state":"idle","status_text":"No game running"},
              "platform":{
                "kind":"android",
                "os_description":"Android 15 (API 35)",
                "cpu_arch":"arm64-v8a",
                "device_model":"POCO F7 Ultra",
                "needs_x86_translation":true
              },
              "storage_bytes":123456789
            }
            """.trimIndent(),
        )
        assertTrue(status.signedIn)
        assertFalse(status.runtimeReady)
        assertEquals(listOf("rootfs", "hangover"), status.missingComponents)
        assertEquals("POCO F7 Ultra", status.platform.deviceModel)
        assertTrue(status.platform.needsX86Translation)
        assertEquals(123456789L, status.storageBytes)
    }

    @Test
    fun `session states map onto the active flag`() {
        fun state(s: String) = json.decodeFromString<SessionSnapshot>("""{"state":"$s"}""")

        assertTrue(state("running").isActive)
        assertTrue(state("starting_vortex").isActive)
        assertTrue(state("preparing_prefix").isActive)
        assertFalse(state("idle").isActive)
        assertFalse(state("exited").isActive)
        assertFalse(state("failed").isActive)
        assertTrue(state("failed").isFailed)
    }

    @Test
    fun `session snapshot keeps the failure explanation`() {
        val snapshot = json.decodeFromString<SessionSnapshot>(
            """
            {
              "state":"failed",
              "game_id":4,
              "status_text":"Game stopped",
              "error":"exited with code 1\n\nWine could not reach an X server.",
              "error_kind":"process",
              "recent_output":["err:module: something"]
            }
            """.trimIndent(),
        )
        assertTrue(snapshot.isFailed)
        assertEquals(4, snapshot.gameId)
        assertTrue(snapshot.error!!.contains("X server"))
        assertEquals(1, snapshot.recentOutput.size)
    }

    @Test
    fun `component status decodes and collapses its multi-line purpose text`() {
        val component = json.decodeFromString<ComponentStatus>(
            """
            {
              "id":"hangover",
              "display_name":"Hangover (ARM64 Wine + FEX)",
              "required":true,
              "installed":false,
              "available_version":"11.16",
              "approx_bytes":307232768,
              "license":"Wine: LGPL-2.1-or-later; FEX: MIT",
              "upstream":"https://github.com/AndreRH/hangover",
              "purpose":"Wine built natively for ARM64,                       plus the emulators."
            }
            """.trimIndent(),
        )
        assertTrue(component.required)
        assertFalse(component.installed)
        assertNull(component.sha256)
        assertFalse("purpose text still contains run-on whitespace", component.purposeText.contains("  "))
        assertTrue(component.purposeText.startsWith("Wine built natively"))
    }

    @Test
    fun `install phases decode from the tagged representation Rust serialises`() {
        fun phase(s: String) = json.decodeFromString<InstallProgress>(s).phase

        assertTrue(phase("""{"component":"rootfs","phase":{"phase":"queued"}}""") is InstallPhase.Queued)
        assertTrue(phase("""{"component":"rootfs","phase":{"phase":"verifying"}}""") is InstallPhase.Verifying)
        assertTrue(phase("""{"component":"rootfs","phase":{"phase":"extracting"}}""") is InstallPhase.Extracting)
        assertTrue(phase("""{"component":"rootfs","phase":{"phase":"done"}}""") is InstallPhase.Done)

        val downloading = phase(
            """{"component":"rootfs","phase":{"phase":"downloading","done":512,"total":2048}}""",
        )
        assertTrue(downloading is InstallPhase.Downloading)
        assertEquals(512L, (downloading as InstallPhase.Downloading).done)
        assertEquals(2048L, downloading.total)

        // A server that sends no Content-Length yields an indeterminate bar.
        val unknownTotal = phase(
            """{"component":"rootfs","phase":{"phase":"downloading","done":512}}""",
        )
        assertNull((unknownTotal as InstallPhase.Downloading).total)

        val configuring = phase(
            """{"component":"hangover","phase":{"phase":"configuring","step":"installing Wine"}}""",
        )
        assertEquals("installing Wine", (configuring as InstallPhase.Configuring).step)

        val failed = phase(
            """{"component":"rootfs","phase":{"phase":"failed","error":"HTTP 404","kind":"network"}}""",
        )
        assertEquals("network", (failed as InstallPhase.Failed).kind)
    }

    @Test
    fun `bridge events distinguish progress from completion`() {
        val progress = json.decodeFromString<BridgeEvent>(
            """{"request_id":7,"kind":"install.progress","data":{"component":"rootfs"}}""",
        )
        assertEquals(7L, progress.requestId)
        assertTrue(progress.kind.endsWith(".progress"))
        assertNull(progress.error)

        val failure = json.decodeFromString<BridgeEvent>(
            """{"request_id":7,"kind":"install","error":"disk full","error_kind":"io"}""",
        )
        assertFalse(failure.kind.endsWith(".progress"))
        assertEquals("io", failure.errorKind)
    }

    @Test
    fun `parsed link exposes the game id but never the token`() {
        val link = json.decodeFromString<ParsedLink>(
            """{"game_id":4,"display":"vortex://play?game=4&token=***"}""",
        )
        assertEquals(4, link.gameId)
        assertFalse("the redacted display must not carry a real token", link.display.contains("token=a"))
        assertTrue(link.display.contains("***"))
    }

    @Test
    fun `config round-trips through the names Rust uses`() {
        val original = json.decodeFromString<TempestConfig>(
            """
            {
              "wine":{"binary":"wine","env":{"DXVK_HUD":"fps"},"windows_version":"win10"},
              "launcher":{"use_fsync":true,"launch_timeout_secs":240},
              "graphics":{"vulkan_driver":"turnip","enable_vkd3d":true,"display":":0"},
              "storage":{"prune_archives_after_install":false}
            }
            """.trimIndent(),
        )
        assertEquals("turnip", original.graphics.vulkanDriver)
        assertTrue(original.launcher.useFsync)
        assertEquals(240L, original.launcher.launchTimeoutSecs)
        assertTrue(original.graphics.enableVkd3d)
        assertFalse(original.storage.pruneArchivesAfterInstall)

        // Re-encoding must produce the same snake_case keys the core expects.
        val encoded = Json.encodeToString(TempestConfig.serializer(), original)
        assertTrue(encoded.contains("\"vulkan_driver\""))
        assertTrue(encoded.contains("\"launch_timeout_secs\""))
        assertTrue(encoded.contains("\"prune_archives_after_install\""))
    }

    @Test
    fun `diagnostics report decodes with fixes attached to failures`() {
        val report = json.decodeFromString<DiagnosticsReport>(
            """
            {
              "checks":[
                {"name":"Container (PRoot)","verdict":"fail","detail":"not found","fix":"Reinstall the APK."},
                {"name":"CPU architecture","verdict":"pass","detail":"arm64-v8a"}
              ],
              "failures":1,
              "warnings":0,
              "platform":{"kind":"android","os_description":"Android 15","cpu_arch":"arm64-v8a","needs_x86_translation":true}
            }
            """.trimIndent(),
        )
        assertEquals(1, report.failures)
        assertNotNull(report.checks.first { it.verdict == "fail" }.fix)
        assertNull(report.checks.first { it.verdict == "pass" }.fix)
    }

    @Test
    fun `an empty catalogue decodes rather than throwing`() {
        assertTrue(json.decodeFromString<GameCatalogue>("""{}""").games.isEmpty())
        assertTrue(json.decodeFromString<GameCatalogue>("""{"games":[],"fetched_at":0}""").games.isEmpty())
    }

    @Test
    fun `unknown fields from a newer core do not break decoding`() {
        // Forward compatibility: adding a field on the Rust side must not brick
        // an older APK.
        val catalogue = json.decodeFromString<GameCatalogue>(
            """{"games":[{"id":1,"name":"G","brand_new_field":true}],"fetched_at":0,"another":1}""",
        )
        assertEquals("G", catalogue.games.single().name)
    }

    @Test
    fun `exception classification drives UI routing`() {
        assertTrue(TempestException("expired", "auth").isAuthFailure)
        assertTrue(TempestException("stopped", "cancelled").isCancellation)
        assertTrue(TempestException("timeout", "network").isTransient)
        assertFalse(TempestException("bad link", "uri").isAuthFailure)
        assertFalse(TempestException("bad link", "uri").isTransient)
    }

    @Test
    fun `relative image urls are already absolute by the time they reach Kotlin`() {
        val catalogue = json.decodeFromString<GameCatalogue>(
            """{"games":[{"id":1,"name":"G","image_url":"https://playvortex.io/static/g.png"}]}""",
        )
        assertTrue(catalogue.games.single().imageUrl!!.startsWith("https://"))
    }

    @Test
    fun `envelope data stays a raw element until the caller types it`() {
        val envelope = json.decodeFromString<Envelope>("""{"ok":true,"data":"initialised"}""")
        assertEquals("initialised", envelope.data!!.jsonPrimitive.content)
    }
}
