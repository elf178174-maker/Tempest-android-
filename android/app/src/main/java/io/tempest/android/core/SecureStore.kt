package io.tempest.android.core

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import android.util.Log
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The Vortex session token at rest.
 *
 * The desktop build encrypts the token with a key in a mode-0600 file, which is
 * reasonable where each user has their own home directory. On Android the
 * stronger primitive is available and is used instead: an AES-256-GCM key
 * generated **inside** the Android Keystore, which never leaves it. The
 * ciphertext lives in ordinary SharedPreferences because on its own it is
 * useless — decrypting needs the hardware-held key, which is bound to this app
 * and wiped if the device's screen lock is removed.
 *
 * StrongBox (a discrete secure element) is used when the device has one, with a
 * silent fall back to the TEE-backed keystore when it does not.
 */
class SecureStore(context: Context) {

    private companion object {
        const val TAG = "SecureStore"
        const val KEYSTORE = "AndroidKeyStore"
        const val KEY_ALIAS = "io.tempest.android.session"
        const val PREFS = "tempest_secure"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val GCM_TAG_BITS = 128
        const val IV_BYTES = 12
    }

    private val prefs = context.applicationContext
        .getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private var usedStrongBox = false

    fun get(key: String): String? {
        val stored = prefs.getString(key, null) ?: return null
        return try {
            val blob = Base64.decode(stored, Base64.NO_WRAP)
            if (blob.size <= IV_BYTES) {
                Log.w(TAG, "stored value for $key is truncated; discarding it")
                delete(key)
                return null
            }
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(
                Cipher.DECRYPT_MODE,
                secretKey(),
                GCMParameterSpec(GCM_TAG_BITS, blob, 0, IV_BYTES),
            )
            String(cipher.doFinal(blob, IV_BYTES, blob.size - IV_BYTES), Charsets.UTF_8)
        } catch (e: Exception) {
            // The usual cause is key invalidation: the user changed or removed
            // the device lock, and the Keystore destroyed the key. Nothing can
            // recover the value, so drop it and let the user sign in again
            // rather than returning a corrupt token that fails confusingly
            // later against the Vortex API.
            Log.w(TAG, "could not decrypt $key; the stored credential is being discarded", e)
            delete(key)
            null
        }
    }

    fun set(key: String, value: String) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, secretKey())
        val iv = cipher.iv
        require(iv.size == IV_BYTES) { "unexpected GCM IV length ${iv.size}" }
        val ciphertext = cipher.doFinal(value.toByteArray(Charsets.UTF_8))
        val blob = iv + ciphertext
        prefs.edit()
            .putString(key, Base64.encodeToString(blob, Base64.NO_WRAP))
            .apply()
    }

    fun delete(key: String) {
        prefs.edit().remove(key).apply()
    }

    fun describe(): String {
        val backing = if (usedStrongBox) "StrongBox" else "TEE or software keystore"
        return "Android Keystore (AES-256-GCM, $backing)"
    }

    private fun secretKey(): SecretKey {
        val keystore = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (keystore.getEntry(KEY_ALIAS, null) as? KeyStore.SecretKeyEntry)?.let {
            return it.secretKey
        }
        return generateKey()
    }

    private fun generateKey(): SecretKey {
        // Try StrongBox first. Not every device has a secure element, and
        // asking for one where it is absent throws rather than degrading, so
        // the fallback is a catch rather than a capability check.
        runCatching { generate(strongBox = true) }
            .onSuccess {
                usedStrongBox = true
                return it
            }
            .onFailure { Log.i(TAG, "StrongBox unavailable; using the standard keystore") }
        return generate(strongBox = false)
    }

    private fun generate(strongBox: Boolean): SecretKey {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        val spec = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            // No per-use authentication: the token has to be readable while a
            // game is running and the screen is off.
            .setUserAuthenticationRequired(false)
            .apply { if (strongBox) setIsStrongBoxBacked(true) }
            .build()
        generator.init(spec)
        return generator.generateKey()
    }
}

/**
 * Where large game payloads are stored.
 *
 * Kept separate from [SecureStore] because it holds a user preference, not a
 * secret. A removable volume is only offered when the device actually has one
 * and it is currently mounted.
 */
class StoragePreferences(private val context: Context) {

    private companion object {
        const val PREFS = "tempest_storage"
        const val KEY_GAMES_DIR = "games_dir"
    }

    private val prefs = context.applicationContext
        .getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /**
     * The chosen directory, or null for the default inside app storage.
     *
     * A stale choice (an SD card that has since been removed) is ignored rather
     * than returned, so the core never receives a path it cannot write to.
     */
    fun gamesDirectory(): String? {
        val stored = prefs.getString(KEY_GAMES_DIR, null) ?: return null
        val dir = java.io.File(stored)
        return if (dir.isDirectory && dir.canWrite()) stored else null
    }

    fun setGamesDirectory(path: String?) {
        prefs.edit().apply {
            if (path == null) remove(KEY_GAMES_DIR) else putString(KEY_GAMES_DIR, path)
        }.apply()
    }

    /**
     * App-specific directories on every mounted volume.
     *
     * These need no runtime permission and are cleaned up when the app is
     * uninstalled, which is the right behaviour for hundreds of megabytes of
     * downloaded runtime.
     */
    fun availableVolumes(): List<StorageVolumeOption> =
        context.getExternalFilesDirs(null)
            .filterNotNull()
            .filter { it.isDirectory || it.mkdirs() }
            .mapIndexed { index, dir ->
                StorageVolumeOption(
                    path = dir.absolutePath,
                    label = if (index == 0) "Internal storage" else "Removable storage",
                    freeBytes = runCatching { dir.usableSpace }.getOrDefault(0L),
                )
            }
}

data class StorageVolumeOption(
    val path: String,
    val label: String,
    val freeBytes: Long,
)
