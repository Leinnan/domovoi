package com.crayen.domovoi.core

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import com.crayen.domovoi.ffi.tunnelGenerateKey
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** What the core's `Store` effect reads and writes. */
interface KeyValueStore {
    fun load(key: String): String?
    fun save(key: String, value: String)
    fun delete(key: String)
}

/** For tests and `DOMOVOI_STORE=memory`-style runs. */
class MemoryStore : KeyValueStore {
    private val values = HashMap<String, String>()
    override fun load(key: String) = values[key]
    override fun save(key: String, value: String) { values[key] = value }
    override fun delete(key: String) { values.remove(key) }
}

/**
 * Tokens, saved servers and drafts, encrypted with an Android Keystore key
 * (AES-GCM) in private preferences. `cache:` keys (the offline thread list)
 * are plain files in the cache folder instead: large, rebuildable, and fine
 * for the system to clear. The app opts out of backup, so a restored phone
 * never clones this device's identity.
 */
class SecureStore(context: Context) : KeyValueStore {
    private val prefs = context.getSharedPreferences("domovoi.secure", Context.MODE_PRIVATE)
    private val cacheDir = File(context.cacheDir, "domovoi")

    override fun load(key: String): String? {
        if (key.startsWith(CACHE)) return cacheFile(key).takeIf { it.exists() }?.readText()
        val sealed = prefs.getString(key, null) ?: return null
        return runCatching { open(sealed) }.getOrNull()
    }

    override fun save(key: String, value: String) {
        if (key.startsWith(CACHE)) {
            cacheDir.mkdirs()
            val file = cacheFile(key)
            val tmp = File(file.path + ".tmp")
            tmp.writeText(value)
            tmp.renameTo(file)
            return
        }
        prefs.edit().putString(key, seal(value)).apply()
    }

    override fun delete(key: String) {
        if (key.startsWith(CACHE)) cacheFile(key).delete() else prefs.edit().remove(key).apply()
    }

    /** This device's iroh identity (64 hex characters), made on first use.
     *  Losing it makes every remote server ask to pair again. */
    fun deviceKey(): String? = runCatching {
        load(DEVICE_KEY) ?: tunnelGenerateKey().also { prefs.edit().putString(DEVICE_KEY, seal(it)).commit() }
    }.getOrNull()

    private fun cacheFile(key: String) = File(cacheDir, key.replace(Regex("[^A-Za-z0-9-]"), "_") + ".json")

    private fun key(): SecretKey {
        val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (keyStore.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    private fun seal(value: String): String {
        val cipher = Cipher.getInstance(TRANSFORM).apply { init(Cipher.ENCRYPT_MODE, key()) }
        val out = cipher.iv + cipher.doFinal(value.toByteArray())
        return Base64.encodeToString(out, Base64.NO_WRAP)
    }

    private fun open(sealed: String): String {
        val bytes = Base64.decode(sealed, Base64.NO_WRAP)
        val cipher = Cipher.getInstance(TRANSFORM)
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes, 0, 12))
        return String(cipher.doFinal(bytes, 12, bytes.size - 12))
    }

    private companion object {
        const val CACHE = "cache:"
        const val DEVICE_KEY = "deviceKey"
        const val ALIAS = "domovoi.store"
        const val TRANSFORM = "AES/GCM/NoPadding"
    }
}
