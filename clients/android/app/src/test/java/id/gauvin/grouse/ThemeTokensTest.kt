// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import androidx.compose.ui.graphics.Color
import id.gauvin.grouse.ui.theme.DarkStatusColors
import id.gauvin.grouse.ui.theme.LightStatusColors
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Pins the design/tokens.json color.semantic.{light,dark}.status mapping that the
 * Android status tokens (Color.kt GrouseStatusColors) are hand-synced against. There is no
 * codegen from tokens.json yet (that gap is recorded in AUDIT.md X-4); this test is the
 * guard so a drift in the hand-synced values is caught, not silently shipped.
 *
 * Values MUST match design/tokens.json:
 *   semantic.light.status = { online #2E7D32, connecting #F5A623, offline #FB4934 }
 *   semantic.dark.status  = { online #3DDC84, connecting #F5A623, offline #FB4934 }
 */
class ThemeTokensTest {

    @Test
    fun lightStatus_matches_tokens() {
        assertEquals(Color(0xFF2E7D32), LightStatusColors.online)
        assertEquals(Color(0xFFF5A623), LightStatusColors.connecting)
        assertEquals(Color(0xFFFB4934), LightStatusColors.offline)
    }

    @Test
    fun darkStatus_matches_tokens() {
        assertEquals(Color(0xFF3DDC84), DarkStatusColors.online)
        assertEquals(Color(0xFFF5A623), DarkStatusColors.connecting)
        assertEquals(Color(0xFFFB4934), DarkStatusColors.offline)
    }
}
