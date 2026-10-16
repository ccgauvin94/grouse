// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Base64
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Biometric app-unlock gate, cryptographically bound to an AndroidKeyStore key.
 *
 * The prompt carries a [BiometricPrompt.CryptoObject] — a cipher over an auth-required AES key —
 * and the unlock only completes if that cipher actually decrypts the stored verifier inside
 * [BiometricPrompt.AuthenticationCallback.onAuthenticationSucceeded]. A bare `onSuccess()` that
 * ignores the result is what code scanning flags (java/android/insecure-local-authentication):
 * the prompt could then be satisfied without the Keystore ever being unlocked.
 *
 * Android forbids a CryptoObject together with DEVICE_CREDENTIAL, so this is BIOMETRIC_STRONG
 * only: on a PIN/pattern-only device [available] returns false and the app is not gated. That is
 * deliberate — the alternative, a device-credential prompt with no CryptoObject, is exactly the
 * weak pattern the scanner rejects.
 */
object Biometric {
    private const val KEYSTORE = "AndroidKeyStore"
    private const val KEY_ALIAS = "grouse_unlock"
    private const val TRANSFORM = "AES/GCM/NoPadding"
    private const val GCM_TAG_BITS = 128
    private const val VERIFIER_BYTES = 32

    // Same store SecureStore writes. The verifier is not a secret (the Keystore key that protects
    // it is), so it does not belong in the encrypted store.
    private const val PREFS = "goose"
    private const val P_CT = "unlock_verifier_ct"
    private const val P_IV = "unlock_verifier_iv"
    private const val P_PLAIN = "unlock_verifier_plain"

    // CryptoObject requires biometric-only auth; DEVICE_CREDENTIAL cannot be combined with it.
    private val AUTH: Int = BiometricManager.Authenticators.BIOMETRIC_STRONG

    /** What one unlock attempt needs: the pre-authentication cipher, and the verifier it acts on.
     *  [decryptCt] is null on the first run after enabling the lock, when the cipher ENCRYPTS the
     *  freshly minted [verifier] instead. */
    private class Check(val cipher: Cipher, val decryptCt: ByteArray?, val verifier: ByteArray)

    /** True only if the user has a usable authenticator enrolled — otherwise we can't gate. */
    fun available(activity: FragmentActivity): Boolean =
        BiometricManager.from(activity).canAuthenticate(AUTH) == BiometricManager.BIOMETRIC_SUCCESS

    private fun keyStore(): KeyStore = KeyStore.getInstance(KEYSTORE).apply { load(null) }

    private fun secretKey(): SecretKey? = keyStore().getKey(KEY_ALIAS, null) as? SecretKey

    private fun clearVerifier(prefs: SharedPreferences) {
        prefs.edit().remove(P_CT).remove(P_IV).remove(P_PLAIN).apply()
    }

    private fun generateKey(): SecretKey {
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        val spec = KeyGenParameterSpec.Builder(
            KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setUserAuthenticationRequired(true)
            // setInvalidatedByBiometricEnrollment defaults true: enrolling a new fingerprint
            // invalidates the key, and the next unlock recreates the verifier. That is the safer
            // default — a newly enrolled finger must not inherit the old unlock. prepare() below
            // detects that invalidation and recovers instead of stranding the user on the lock.
            .apply {
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    // Auth per use, strong biometric only — the same class the prompt allows.
                    setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
                } else {
                    @Suppress("DEPRECATION")
                    setUserAuthenticationValidityDurationSeconds(-1)
                }
            }
            .build()
        gen.init(spec)
        return gen.generateKey()
    }

    /** Build the cipher for one unlock, regenerating the key if it is gone or was invalidated. */
    private fun prepare(prefs: SharedPreferences): Check? {
        return try {
            check(secretKey(), prefs)
        } catch (e: KeyPermanentlyInvalidatedException) {
            // A new biometric enrollment killed the key; it cannot be used again. Recreate it and
            // start the verifier over rather than failing the unlock forever.
            runCatching { keyStore().deleteEntry(KEY_ALIAS) }
            clearVerifier(prefs)
            try {
                check(generateKey(), prefs)
            } catch (e2: Exception) {
                null
            }
        } catch (e: Exception) {
            null
        }
    }

    private fun check(key: SecretKey?, prefs: SharedPreferences): Check {
        val k = key ?: generateKey().also {
            // A fresh key (first run, or a wiped verifier) cannot read the old ciphertext.
            clearVerifier(prefs)
        }
        val ct = prefs.getString(P_CT, null)?.let { Base64.decode(it, Base64.NO_WRAP) }
        val iv = prefs.getString(P_IV, null)?.let { Base64.decode(it, Base64.NO_WRAP) }
        val plain = prefs.getString(P_PLAIN, null)?.let { Base64.decode(it, Base64.NO_WRAP) }
        val cipher = Cipher.getInstance(TRANSFORM)
        return if (ct != null && iv != null && plain != null) {
            cipher.init(Cipher.DECRYPT_MODE, k, GCMParameterSpec(GCM_TAG_BITS, iv))
            Check(cipher, ct, plain)
        } else {
            // First unlock after enabling the lock: mint the verifier under the same auth-required
            // key, inside the authenticated prompt (an auth-per-use key cannot be used to encrypt
            // before the user authenticates either).
            cipher.init(Cipher.ENCRYPT_MODE, k)
            Check(cipher, null, ByteArray(VERIFIER_BYTES).also { SecureRandom().nextBytes(it) })
        }
    }

    fun prompt(activity: FragmentActivity, onSuccess: () -> Unit, onFail: (String) -> Unit) {
        val prefs = activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val check = prepare(prefs)
        if (check == null) {
            onFail("Can't set up the biometric key.")
            return
        }

        val prompt = BiometricPrompt(
            activity, ContextCompat.getMainExecutor(activity),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    val cipher = result.cryptoObject?.cipher
                    if (cipher == null) {
                        onFail("Authentication produced no crypto object.")
                        return
                    }
                    try {
                        if (check.decryptCt != null) {
                            val plain = cipher.doFinal(check.decryptCt)
                            if (plain.contentEquals(check.verifier)) onSuccess()
                            else onFail("Unlock verification failed.")
                        } else {
                            val ct = cipher.doFinal(check.verifier)
                            prefs.edit()
                                .putString(P_CT, Base64.encodeToString(ct, Base64.NO_WRAP))
                                .putString(P_IV, Base64.encodeToString(cipher.iv, Base64.NO_WRAP))
                                .putString(P_PLAIN, Base64.encodeToString(check.verifier, Base64.NO_WRAP))
                                .apply()
                            onSuccess()
                        }
                    } catch (e: Exception) {
                        onFail("Unlock verification failed: ${e.message}")
                    }
                }

                override fun onAuthenticationError(code: Int, msg: CharSequence) = onFail(msg.toString())
            },
        )
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle("Unlock Grouse")
            .setSubtitle("Authenticate to reach your agent")
            .setAllowedAuthenticators(AUTH)
            // Required: with no DEVICE_CREDENTIAL fallback, PromptInfo demands a negative button.
            .setNegativeButtonText("Cancel")
            .build()
        prompt.authenticate(info, BiometricPrompt.CryptoObject(check.cipher))
    }
}
