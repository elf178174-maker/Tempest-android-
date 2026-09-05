package io.tempest.android

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import io.tempest.android.core.SecureStore
import io.tempest.android.core.StoragePreferences
import io.tempest.android.core.TempestBridge
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Device-side checks for the parts that cannot be exercised on the JVM: the
 * Keystore, the manifest's intent filter, and the fact that the native library
 * is actually present in the built APK.
 */
@RunWith(AndroidJUnit4::class)
class DeepLinkInstrumentedTest {

    private val context: Context get() = ApplicationProvider.getApplicationContext()

    @Test
    fun packageIsWhatWeExpect() {
        assertTrue(context.packageName.startsWith("io.tempest.android"))
    }

    /**
     * The app must be the handler for `vortex://`. If this regresses, tapping
     * Play on the Vortex website silently does nothing.
     */
    @Test
    fun vortexSchemeResolvesToThisApp() {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse("vortex://play?game=1&token=abc"))
        val matches = context.packageManager.queryIntentActivities(intent, 0)
        assertTrue(
            "no activity handles vortex:// links",
            matches.any { it.activityInfo.packageName == context.packageName },
        )
    }

    /**
     * The native library has to be in `nativeLibraryDir`, which is the only
     * directory this app is allowed to execute from.
     */
    @Test
    fun nativeLibraryDirectoryExistsAndIsPopulated() {
        val dir = java.io.File(context.applicationInfo.nativeLibraryDir)
        assertTrue("nativeLibraryDir does not exist: $dir", dir.isDirectory)
        val names = dir.list()?.toList().orEmpty()
        assertTrue(
            "the Rust core is missing from the APK; found: $names",
            names.contains("libtempest_jni.so"),
        )
    }

    @Test
    fun coreInitialisesAndReportsThisDevice() {
        val result = TempestBridge.initialise(context)
        assertTrue("core failed to start: ${result.exceptionOrNull()}", result.isSuccess)
        assertTrue(TempestBridge.isInitialised)
    }

    /**
     * A round trip through the hardware-backed keystore, including the case
     * that actually bites users: a value written, read back, and then removed.
     */
    @Test
    fun secureStoreRoundTripsAndDeletes() {
        val store = SecureStore(context)
        val key = "test.session.token"
        val secret = "a-session-token-value-1234567890"

        store.set(key, secret)
        assertEquals(secret, store.get(key))

        // Overwriting must replace, not append.
        store.set(key, "second")
        assertEquals("second", store.get(key))

        store.delete(key)
        assertNull(store.get(key))
        // Deleting something absent is not an error.
        store.delete(key)

        assertTrue(store.describe().contains("Keystore"))
    }

    @Test
    fun secureStoreCiphertextIsNotThePlaintext() {
        val store = SecureStore(context)
        val key = "test.ciphertext.check"
        val secret = "PLAINTEXT-SHOULD-NOT-APPEAR"
        store.set(key, secret)

        val prefs = context.getSharedPreferences("tempest_secure", Context.MODE_PRIVATE)
        val stored = prefs.getString(key, null)
        assertNotNull(stored)
        assertFalse("the token was stored in the clear", stored!!.contains(secret))

        store.delete(key)
    }

    @Test
    fun storageVolumesAreReportedAndWritable() {
        val volumes = StoragePreferences(context).availableVolumes()
        assertTrue("no app-specific external directory was reported", volumes.isNotEmpty())
        volumes.forEach {
            assertTrue("${it.path} is not writable", java.io.File(it.path).canWrite())
        }
    }

    @Test
    fun aStaleGamesDirectoryChoiceIsIgnored() {
        val prefs = StoragePreferences(context)
        val original = prefs.gamesDirectory()
        try {
            // Simulates an SD card that has been removed since the user chose it.
            prefs.setGamesDirectory("/storage/definitely-not-mounted/tempest")
            assertNull(
                "a path that no longer exists must not be handed to the core",
                prefs.gamesDirectory(),
            )
        } finally {
            prefs.setGamesDirectory(original)
        }
    }
}
