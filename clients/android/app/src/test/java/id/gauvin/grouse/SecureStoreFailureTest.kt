// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.security.GeneralSecurityException
import java.security.UnrecoverableKeyException
import javax.crypto.AEADBadTagException

/**
 * Wiping goose_secure loses the X-Secret-Key and the iroh identity, so only a positively
 * identified dead keyset may trigger it. Everything else must stay retryable. The Android
 * Keystore exception classes are mirrored by same-named fakes here: this is a JVM test and
 * the mockable android.jar constructors throw "Stub!".
 */
class SecureStoreFailureTest {
    private class KeyPermanentlyInvalidatedException : GeneralSecurityException()
    private class KeyExpiredException : GeneralSecurityException()
    private class KeyNotYetValidException : GeneralSecurityException()

    @Test
    fun `a generic security exception is retryable`() {
        assertFalse(isUnrecoverableKeysetFailure(GeneralSecurityException("Keystore busy")))
        assertFalse(isUnrecoverableKeysetFailure(GeneralSecurityException()))
    }

    @Test
    fun `a plain exception or IO error is retryable`() {
        assertFalse(isUnrecoverableKeysetFailure(IllegalStateException("bad prefs state")))
        assertFalse(isUnrecoverableKeysetFailure(java.io.IOException("disk I/O")))
    }

    @Test
    fun `a key that is not valid yet is retryable`() {
        assertFalse(isUnrecoverableKeysetFailure(KeyNotYetValidException()))
    }

    @Test
    fun `a permanently invalidated key is unrecoverable`() {
        assertTrue(isUnrecoverableKeysetFailure(KeyPermanentlyInvalidatedException()))
    }

    @Test
    fun `an expired key is unrecoverable`() {
        assertTrue(isUnrecoverableKeysetFailure(KeyExpiredException()))
    }

    @Test
    fun `unrecoverable key and ciphertext integrity failures are unrecoverable`() {
        assertTrue(isUnrecoverableKeysetFailure(UnrecoverableKeyException("gone")))
        assertTrue(isUnrecoverableKeysetFailure(AEADBadTagException()))
    }

    @Test
    fun `an unambiguous invalidation message is enough`() {
        assertTrue(isUnrecoverableKeysetFailure(
            GeneralSecurityException("Tink key permanently invalidated; re-onboard")))
        assertTrue(isUnrecoverableKeysetFailure(
            GeneralSecurityException("the key was invalidated")))
    }

    @Test
    fun `classification walks the cause chain`() {
        val wrapped = GeneralSecurityException("failed to decrypt value", KeyPermanentlyInvalidatedException())
        assertTrue(isUnrecoverableKeysetFailure(wrapped))
        val transient = GeneralSecurityException("outer", IllegalStateException("inner"))
        assertFalse(isUnrecoverableKeysetFailure(transient))
    }

    @Test
    fun `an unrelated message never wipes the store`() {
        assertFalse(isUnrecoverableKeysetFailure(
            GeneralSecurityException("unable to decrypt value: keystore temporarily unavailable")))
    }
}
