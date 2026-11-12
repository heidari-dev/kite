package io.github.heidaridev.kite

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.AtomicFile
import io.github.heidaridev.kite.ffi.generateKey
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The relay's private key and the paired computers' public keys, in files only this app can read
 * and that backups skip. The private key is stored encrypted with an AES key that stays inside
 * Android Keystore.
 */
internal object Store {

    private const val ALIAS = "relay-key-wrap"
    private const val IV = 12
    private const val KEY = 32

    fun privateKey(context: Context): ByteArray {
        val file = AtomicFile(File(context.noBackupFilesDir, "relay.key"))
        if (file.baseFile.exists()) return decrypt(file.readFully())
        val key = generateKey()
        write(file, encrypt(key))
        return key
    }

    fun peers(context: Context): List<ByteArray> {
        val file = AtomicFile(File(context.noBackupFilesDir, "peers"))
        if (!file.baseFile.exists()) return emptyList()
        // A file of the wrong size is not ours to guess at; start without paired computers.
        val data = file.readFully().takeIf { it.size % KEY == 0 } ?: return emptyList()
        return (data.indices step KEY).map { data.copyOfRange(it, it + KEY) }
    }

    fun savePeers(context: Context, peers: List<ByteArray>) {
        write(AtomicFile(File(context.noBackupFilesDir, "peers")), peers.fold(ByteArray(0)) { all, k -> all + k })
    }

    private fun write(file: AtomicFile, data: ByteArray) {
        val out = file.startWrite()
        try {
            out.write(data)
            file.finishWrite(out)
        } catch (e: Exception) {
            file.failWrite(out)
            throw e
        }
    }

    private fun encrypt(plain: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, wrappingKey())
        return cipher.iv + cipher.doFinal(plain)
    }

    private fun decrypt(data: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, wrappingKey(), GCMParameterSpec(128, data, 0, IV))
        return cipher.doFinal(data, IV, data.size - IV)
    }

    private fun wrappingKey(): SecretKey {
        val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (keyStore.getKey(ALIAS, null) as? SecretKey)?.let { return it }
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
}
