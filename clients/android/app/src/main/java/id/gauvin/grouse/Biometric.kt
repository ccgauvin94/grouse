// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import android.os.Build
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity

/** Thin wrapper over BiometricPrompt used to gate app unlock. */
object Biometric {
    // STRONG (Class 3) only — WEAK admits spoofable 2D-face sensors, and this gate protects the
    // RCE-capable secret key. Add device-credential (PIN/pattern) as a fallback on API 30+, where
    // BiometricPrompt supports the combo, so PIN-only devices are still gated. (<30 the combo is
    // unsupported for setAllowedAuthenticators, so we require a strong biometric there.)
    private val AUTH: Int = BiometricManager.Authenticators.BIOMETRIC_STRONG or
        (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R)
            BiometricManager.Authenticators.DEVICE_CREDENTIAL else 0)

    private val hasCredentialFallback get() = Build.VERSION.SDK_INT >= Build.VERSION_CODES.R

    /** True only if the user has a usable authenticator enrolled — otherwise we can't gate. */
    fun available(activity: FragmentActivity): Boolean =
        BiometricManager.from(activity).canAuthenticate(AUTH) == BiometricManager.BIOMETRIC_SUCCESS

    fun prompt(activity: FragmentActivity, onSuccess: () -> Unit, onFail: (String) -> Unit) {
        val prompt = BiometricPrompt(
            activity, ContextCompat.getMainExecutor(activity),
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = onSuccess()
                override fun onAuthenticationError(code: Int, msg: CharSequence) = onFail(msg.toString())
            }
        )
        val info = BiometricPrompt.PromptInfo.Builder()
            .setTitle("Unlock Grouse")
            .setSubtitle("Authenticate to reach your agent")
            .setAllowedAuthenticators(AUTH)
            .apply {
                // A negative button is disallowed when DEVICE_CREDENTIAL is offered (the credential
                // screen is its own fallback); required otherwise.
                if (!hasCredentialFallback) setNegativeButtonText("Cancel")
            }
            .build()
        prompt.authenticate(info)
    }
}
