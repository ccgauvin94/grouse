// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.grouse_core.ItemKind

/**
 * The rich item stream is Android's only transcript channel now
 * (docs/TRANSCRIPT_MODEL.md phase 3). A kind mapped to the wrong role is exactly
 * how the migration would silently break rendering (a chart as a plain tool row,
 * an MCP app as a chip), so pin the mapping.
 */
class ItemRoleTest {

    @Test
    fun `every item kind maps to the role the bubble delegate renders`() {
        assertEquals("user", itemRole(ItemKind.USER))
        assertEquals("assistant", itemRole(ItemKind.AGENT))
        assertEquals("thought", itemRole(ItemKind.THOUGHT))
        assertEquals("error", itemRole(ItemKind.ERROR))
        assertEquals("tool", itemRole(ItemKind.TOOL))
        // A toolgroup renders as tool rows: it expands to one per call, and the
        // chat UI re-groups consecutive tool rows itself.
        assertEquals("tool", itemRole(ItemKind.TOOL_GROUP))
        assertEquals("chart", itemRole(ItemKind.CHART))
        assertEquals("mcpapp", itemRole(ItemKind.MCP_APP))
    }
}
