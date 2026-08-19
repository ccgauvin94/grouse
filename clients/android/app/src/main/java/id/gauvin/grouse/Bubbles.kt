// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

import id.gauvin.grouse.ui.theme.GrouseShapes
import android.content.Context
import android.graphics.Bitmap
import android.net.Uri
import android.util.Base64
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.shrinkVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.filled.ArrowUpward
import androidx.compose.material.icons.filled.Delete
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.runtime.snapshotFlow
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material.icons.filled.Public
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Chat
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.ArrowDropDown
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.Image
import androidx.compose.material.icons.filled.InsertDriveFile
import androidx.compose.material.icons.filled.PhotoCamera
import androidx.compose.material.icons.filled.Menu
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.Send
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.*
import androidx.compose.foundation.Image
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import android.widget.Toast
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.navigation.NavController
import com.halilibo.richtext.markdown.Markdown
import com.halilibo.richtext.ui.material3.RichText
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.contentOrNull

@Composable
fun MessageBubble(m: ChatMessage, streaming: Boolean = false, usage: AcpEvent.MessageUsage? = null) {
    when (m.role) {
        "user" -> UserBubble(m)
        "thought" -> ThoughtBubble(m.text)
        "tool" -> ToolChip(m)
        "error" -> ErrorBubble(m.text)
        "chart" -> ChartView(m.text)
        "mcpapp" -> McpAppView(m)
        else -> AssistantBubble(m.text, streaming, usage)
    }
}

/**
 * Renders a server-hosted MCP-App template (autovisualiser chart/sankey/radar/donut/treemap/
 * chord/map/mermaid — anything whose tool_call carries _meta.goose.mcpApp).
 *
 * The template expects to live in an IFRAME and speak JSON-RPC over postMessage to its parent
 * (see goose's mcp-app-bridge.js): it requests `ui/initialize`, announces `initialized`, then
 * waits for a `ui/notifications/tool-input` carrying the tool's arguments, and reports its
 * rendered height via `ui/notifications/size-changed`. A bare WebView can't be that parent —
 * window.parent === window at the top level, so the guest's messages would loop back to
 * itself. Hence the tiny HOST page: it iframes the guest via srcdoc (same-origin, so
 * contentWindow is reachable), relays the protocol, and forwards height changes to Compose
 * through a JS interface so the bubble grows to fit. goose's own /mcp-app-proxy route is
 * loopback-only and can never serve a phone, which is why this is done client-side at all.
 */
@android.annotation.SuppressLint("SetJavaScriptEnabled")
@Composable
private fun McpAppView(m: ChatMessage) {
    if (m.appHtml.isEmpty()) { ToolChip(m); return }   // fetch in flight, or failed: stay a tool row
    val heightDp = remember(m.id) { androidx.compose.runtime.mutableIntStateOf(240) }
    val dark = androidx.compose.foundation.isSystemInDarkTheme()
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(10.dp),
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
    ) {
        AndroidView(
            factory = { ctx ->
                android.webkit.WebView(ctx).apply {
                    settings.javaScriptEnabled = true
                    settings.domStorageEnabled = true
                    setBackgroundColor(android.graphics.Color.TRANSPARENT)
                    isVerticalScrollBarEnabled = false
                    addJavascriptInterface(object {
                        @android.webkit.JavascriptInterface fun guestHtml() = m.appHtml
                        @android.webkit.JavascriptInterface fun toolInput() = m.detail.ifBlank { "{}" }
                        @android.webkit.JavascriptInterface fun theme() = if (dark) "dark" else "light"
                        @android.webkit.JavascriptInterface fun sizeChanged(h: Int) {
                            android.os.Handler(android.os.Looper.getMainLooper()).post {
                                heightDp.intValue = h.coerceIn(120, 640)
                            }
                        }
                        @android.webkit.JavascriptInterface fun log(msg: String) {
                            android.util.Log.w("McpApp", msg)
                        }
                    }, "GrouseHost")
                    // Guest console (iframe included) lands in logcat under "McpApp" too:
                    // adb logcat -s McpApp
                    webChromeClient = object : android.webkit.WebChromeClient() {
                        override fun onConsoleMessage(c: android.webkit.ConsoleMessage): Boolean {
                            android.util.Log.w("McpApp", "${c.messageLevel()} ${c.message()}")
                            return true
                        }
                    }
                    loadDataWithBaseURL(null, MCP_APP_HOST, "text/html", "utf-8", null)
                }
            },
            modifier = Modifier.fillMaxWidth().height(heightDp.intValue.dp).padding(8.dp)
        )
    }
}

/** Host page for MCP-App guests: iframe + postMessage relay. See McpAppView. */
private val MCP_APP_HOST = """
<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'unsafe-inline' 'wasm-unsafe-eval'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; form-action 'none'; base-uri 'none'">
<style>html,body{margin:0;padding:0;background:transparent}iframe{border:0;width:100%;display:block;height:224px}</style>
</head><body><iframe id="f" sandbox="allow-scripts"></iframe><script>
(function(){
  var f = document.getElementById('f');
  function reply(msg){ if (f.contentWindow) f.contentWindow.postMessage(msg, '*'); }
  window.addEventListener('message', function(ev){
    var m = ev.data || {};
    if (m.method === 'ui/initialize' && m.id != null) {
      reply({jsonrpc:'2.0', id:m.id,
             result:{hostContext:{theme:GrouseHost.theme(), displayMode:'inline'}}});
    } else if (m.method === 'ui/notifications/initialized') {
      var input; try { input = JSON.parse(GrouseHost.toolInput()); } catch(e) { input = {}; }
      reply({jsonrpc:'2.0', method:'ui/notifications/tool-input', params:{arguments:input}});
    } else if (m.method === 'ui/notifications/size-changed') {
      var h = (m.params && m.params.height) || 0;
      if (h > 0) { f.style.height = h + 'px'; GrouseHost.sizeChanged(Math.round(h)); }
    } else if (m.id != null && m.method) {
      // Answer anything else (display-mode requests etc.) with an empty result so no
      // guest awaits a reply forever.
      reply({jsonrpc:'2.0', id:m.id, result:{}});
    }
  });
  f.addEventListener('load', function(){
    try {
      var w = f.contentWindow;
      // Surface guest failures — an iframe error is otherwise a silent blank chart.
      w.addEventListener('error', function(e){ GrouseHost.log('guest: ' + e.message); });
      // Animations run on rAF, and rAF inside a nested srcdoc iframe stalls on some
      // Android WebViews — Chart.js then paints axes/legend but freezes the data
      // elements at t=0, i.e. a fully-drawn EMPTY chart. Static render sidesteps it.
      if (w.Chart && w.Chart.defaults) w.Chart.defaults.animation = false;
    } catch(e) { GrouseHost.log('hook: ' + e); }
  });
  // Guest gets sandbox="allow-scripts" (no allow-same-origin): scripts + postMessage
  // work, but the guest has an opaque origin — no parent-DOM access, no localStorage,
  // no cookies. The host CSP (above) blocks any exfil/navigation the bridge grows into.
  f.srcdoc = GrouseHost.guestHtml();
})();
</script></body></html>
""".trimIndent()

/** Renders an autovisualiser chart spec (Chart.js JSON) in a WebView with bundled Chart.js. */
@android.annotation.SuppressLint("SetJavaScriptEnabled")
@Composable
private fun ChartView(spec: String) {
    Surface(
        color = MaterialTheme.colorScheme.surface,
        shape = RoundedCornerShape(10.dp),
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)
    ) {
        AndroidView(
            factory = { ctx ->
                android.webkit.WebView(ctx).apply {
                    settings.javaScriptEnabled = true
                    setBackgroundColor(android.graphics.Color.TRANSPARENT)
                    isVerticalScrollBarEnabled = false
                    isHorizontalScrollBarEnabled = false
                    loadDataWithBaseURL("file:///android_asset/", chartHtml(spec), "text/html", "utf-8", null)
                }
            },
            modifier = Modifier.fillMaxWidth().height(240.dp).padding(8.dp)
        )
    }
}

internal fun chartHtml(spec: String): String =
    CHART_TEMPLATE.replace(
        "__SPEC__",
        org.json.JSONObject.quote(spec)
            .replace("<", "\\u003c") // JSON-encode + neutralize `<` so a spec embedding `</script>` can't
                                     // terminate the template's script element (S-A-4).
    )

private val CHART_TEMPLATE = """
<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1">
<script src="chart.min.js"></script>
<style>html,body{margin:0;padding:0;background:transparent}.wrap{position:relative;height:224px}</style></head>
<body><div class="wrap"><canvas id="c"></canvas></div><script>
try {
  const spec = __SPEC__;
  const dark = matchMedia('(prefers-color-scheme: dark)').matches;
  Chart.defaults.color = dark ? '#e2e2e2' : '#303030';
  Chart.defaults.borderColor = dark ? '#404040' : '#e2e2e2';
  new Chart(document.getElementById('c'), {
    type: spec.type || 'bar',
    data: { labels: spec.labels || [], datasets: spec.datasets || [] },
    options: { responsive: true, maintainAspectRatio: false,
      plugins: { title: { display: !!spec.title, text: spec.title || '' },
                 legend: { display: (spec.datasets||[]).length > 1 } } }
  });
} catch(e) { document.body.innerHTML = '<pre style="color:#c0392b">chart error: '+e+'</pre>'; }
</script></body></html>
""".trimIndent()

@Composable
private fun Markdownish(text: String) {
    RichText(modifier = Modifier.fillMaxWidth()) { Markdown(text) }
}

/** Long-press any message to copy its text. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
/** Long-press to copy. Still used by USER bubbles, which have no generation stats -- an
 *  assistant bubble opens a menu instead, because there is something to show alongside copy.
 *  A menu whose only item is Copy would be strictly worse than copying. */
private fun Modifier.copyOnLongPress(text: String): Modifier {
    val clip = LocalClipboardManager.current
    val ctx = LocalContext.current
    return combinedClickable(onClick = {}, onLongClick = {
        clip.setText(AnnotatedString(text)); Toast.makeText(ctx, "Copied", Toast.LENGTH_SHORT).show()
    })
}

@Composable
private fun UserBubble(m: ChatMessage) {
    Row(Modifier.fillMaxWidth().padding(vertical = 4.dp), horizontalArrangement = Arrangement.End) {
        Surface(
            color = MaterialTheme.colorScheme.primaryContainer,
            contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
            shape = GrouseShapes.userBubble,
            modifier = Modifier.widthIn(max = 320.dp)
        ) {
            Column(Modifier.copyOnLongPress(m.text).padding(horizontal = 6.dp, vertical = 6.dp)) {
                m.images.forEach { img ->
                    val decoded by androidx.compose.runtime.produceState<androidx.compose.ui.graphics.ImageBitmap?>(
                        imageDecodeCache[img.dataB64], img.dataB64
                    ) {
                        // Decode off the main thread; never decode the same payload twice.
                        if (value == null) {
                            value = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
                                cachedDecodeImage(img.dataB64)
                            }
                        }
                    }
                    val bmp = decoded
                    if (bmp != null) Image(
                        bitmap = bmp, contentDescription = "attached image",
                        contentScale = ContentScale.Fit,
                        modifier = Modifier.padding(bottom = if (m.text.isNotBlank()) 6.dp else 0.dp)
                            .heightIn(max = 220.dp).clip(RoundedCornerShape(10.dp)))
                }
                if (m.text.isNotBlank())
                    Box(Modifier.padding(horizontal = 6.dp, vertical = 2.dp)) { Markdownish(m.text) }
            }
        }
    }
}

/**
 * Bounded in-memory cache for decoded chat images (A-9). Content-addressed by the
 * base64 payload so identical images across messages decode once. A simple cap
 * evicts everything when full — decoding is cheap and correct; the cap only bounds
 * memory. java.util.concurrent.ConcurrentHashMap (no new dependency).
 */
private const val IMAGE_DECODE_CACHE_MAX = 16
private val imageDecodeCache =
    java.util.concurrent.ConcurrentHashMap<String, androidx.compose.ui.graphics.ImageBitmap>()

/** Decode an ImageBlock's base64 (NO_WRAP) payload to an ImageBitmap for the chat bubble. */
private fun decodeImageBlock(b64: String): androidx.compose.ui.graphics.ImageBitmap? = try {
    val bytes = android.util.Base64.decode(b64, android.util.Base64.DEFAULT)
    android.graphics.BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.asImageBitmap()
} catch (e: Exception) { null }

/** decodeImageBlock + content-addressed cache (main-thread safe; call from a background dispatcher). */
private fun cachedDecodeImage(b64: String): androidx.compose.ui.graphics.ImageBitmap? {
    imageDecodeCache[b64]?.let { return it }
    val bmp = decodeImageBlock(b64) ?: return null
    if (imageDecodeCache.size >= IMAGE_DECODE_CACHE_MAX) imageDecodeCache.clear()
    imageDecodeCache[b64] = bmp
    return bmp
}

/** tok/s, TTFT and cost for one reply, or null if none were recorded. */
private fun usageLine(u: AcpEvent.MessageUsage): String {
    val bits = mutableListOf<String>()
    if (u.elapsedMs > 0) bits += "%.1f tok/s".format(u.outputTokens * 1000.0 / u.elapsedMs)
    bits += "${u.outputTokens} tokens"
    if (u.ttftMs > 0) bits += "TTFT ${u.ttftMs}ms"
    u.cost?.let { if (it > 0) bits += "$" + "%.4f".format(it) }
    return bits.joinToString("  ·  ")
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun AssistantBubble(text: String, streaming: Boolean = false, usage: AcpEvent.MessageUsage? = null) {
    // Stats used to sit permanently under the newest reply. They are diagnostics -- interesting
    // when you are asking "why was that slow", noise the rest of the time, and they moved the
    // conversation around as they appeared. Long-press surfaces them, alongside the copy action
    // that long-press already did, so nothing that was reachable stopped being reachable.
    var menu by remember { mutableStateOf(false) }
    val clip = LocalClipboardManager.current
    val ctx = LocalContext.current
    Column(Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Box {
            Box(
                Modifier
                    .combinedClickable(onClick = {}, onLongClick = { menu = true })
                    .padding(horizontal = 2.dp, vertical = 4.dp)
            ) {
                // Plain text while streaming — re-parsing the growing Markdown every token is
                // O(n²). The bubble re-renders once with full Markdown when the turn finishes.
                if (streaming) Text(text) else Markdownish(text)
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                Text(
                    usage?.let(::usageLine) ?: "No stats recorded for this message",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.outline,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                )
                HorizontalDivider()
                DropdownMenuItem(text = { Text(stringResource(R.string.copy_text)) }, onClick = {
                    menu = false
                    clip.setText(AnnotatedString(text))
                    Toast.makeText(ctx, "Copied", Toast.LENGTH_SHORT).show()
                })
            }
        }
    }
}

@Composable
private fun ThoughtBubble(text: String) {
    var expanded by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
        Row(
            Modifier.clickable { expanded = !expanded }.padding(vertical = 4.dp),
            verticalAlignment = Alignment.CenterVertically
        ) {
            Icon(
                if (expanded) Icons.Filled.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight,
                contentDescription = null, modifier = Modifier.size(18.dp),
                tint = MaterialTheme.colorScheme.outline
            )
            Icon(Icons.Filled.Psychology, contentDescription = null, modifier = Modifier.size(16.dp),
                tint = MaterialTheme.colorScheme.outline)
            Spacer(Modifier.width(6.dp))
            Text(stringResource(R.string.thinking), style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.outline)
        }
        AnimatedVisibility(expanded) {
            Text(text, style = MaterialTheme.typography.bodySmall.copy(fontStyle = FontStyle.Italic),
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(start = 24.dp, bottom = 4.dp))
        }
    }
}

/** A run of consecutive same-role messages for display: a lone message, or 2+ consecutive tool
 *  calls collapsed into one dropdown (goose often fires 5-10 shell/read calls back to back — a
 *  wall of individual chips otherwise, see the "grouping" ask). */
internal sealed interface ChatItem {
    val firstId: Long
    data class Msg(val m: ChatMessage) : ChatItem { override val firstId get() = m.id }
    data class Tools(val items: List<ChatMessage>) : ChatItem { override val firstId get() = items.first().id }
}

/** Walk messages in chronological order, merging consecutive role=="tool" runs of 2+ into one
 *  ChatItem.Tools; everything else (including a lone tool call) stays a ChatItem.Msg. */
internal fun groupChatItems(messages: List<ChatMessage>): List<ChatItem> {
    val out = mutableListOf<ChatItem>()
    var i = 0
    while (i < messages.size) {
        val m = messages[i]
        if (m.role == "tool") {
            var j = i + 1
            while (j < messages.size && messages[j].role == "tool") j++
            out += if (j - i > 1) ChatItem.Tools(messages.subList(i, j).toList()) else ChatItem.Msg(m)
            i = j
        } else {
            out += ChatItem.Msg(m); i++
        }
    }
    return out
}

/** ACP tool titles look like "shell · ls -laR /some/long/path". Split into a short badge label
 *  and the (often long) detail, so a collapsed group header can show just the label. */
private fun splitToolTitle(title: String): Pair<String, String> {
    val idx = title.indexOf(" · ")
    return if (idx >= 0) title.take(idx) to title.substring(idx + 3) else title to ""
}

/** Collapsed: one chip, "N× <tool>" (or "N tool calls" for a mixed run). Tap to expand into the
 *  individual calls, oldest first, each rendered like a normal ToolChip. */
@Composable
internal fun ToolChipGroup(items: List<ChatMessage>) {
    var expanded by remember { mutableStateOf(false) }
    val names = items.map { splitToolTitle(it.text).first }.distinct()
    val label = if (names.size == 1) "${items.size}× ${names[0]}" else "${items.size} tool calls"
    Column(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
        Surface(
            color = MaterialTheme.colorScheme.secondaryContainer.copy(alpha = 0.55f),
            contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
            shape = GrouseShapes.toolChip,
            modifier = Modifier.clickable { expanded = !expanded }
        ) {
            Row(Modifier.padding(horizontal = 12.dp, vertical = 7.dp), verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    if (expanded) Icons.Filled.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight,
                    contentDescription = null, modifier = Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.outline
                )
                Spacer(Modifier.width(2.dp))
                Icon(Icons.Filled.Build, contentDescription = null, modifier = Modifier.size(13.dp),
                    tint = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.width(8.dp))
                Text(label.replace('_', ' '), style = MaterialTheme.typography.labelLarge)
            }
        }
        AnimatedVisibility(expanded) {
            Column(Modifier.padding(start = 20.dp, top = 2.dp)) {
                items.forEach { ToolChip(it) }
            }
        }
    }
}

/** Tap to expand and see the tool's rawInput (command/args) — Desktop shows this inline; here it's
 *  collapsed by default (matches ThoughtBubble's pattern) so a wall of tool calls stays scannable. */
@Composable
private fun ToolChip(m: ChatMessage) {
    val title = m.text; val detail = m.detail
    var expanded by remember { mutableStateOf(false) }
    val (name, inlineDetail) = splitToolTitle(title)
    val hasBody = detail.isNotBlank() || m.output.isNotBlank()
    Column(Modifier.fillMaxWidth().padding(vertical = 3.dp)) {
        Surface(color = MaterialTheme.colorScheme.secondaryContainer.copy(alpha = 0.55f),
            contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
            shape = GrouseShapes.toolChip,
            modifier = Modifier.let { if (hasBody) it.clickable { expanded = !expanded } else it }) {
            Row(Modifier.padding(horizontal = 12.dp, vertical = 7.dp),
                verticalAlignment = Alignment.CenterVertically) {
                when (m.status) {
                    // Live lifecycle from tool_call_update: spinner while running, error tint on
                    // failure, plain wrench once done (or for replayed history, which has no status).
                    "in_progress" -> CircularProgressIndicator(Modifier.size(13.dp), strokeWidth = 1.5.dp)
                    "failed" -> Icon(Icons.Filled.Close, contentDescription = "failed",
                        modifier = Modifier.size(13.dp), tint = MaterialTheme.colorScheme.error)
                    else -> Icon(Icons.Filled.Build, contentDescription = null,
                        modifier = Modifier.size(13.dp), tint = MaterialTheme.colorScheme.primary)
                }
                Spacer(Modifier.width(8.dp))
                Text(name.replace('_', ' '), style = MaterialTheme.typography.labelLarge)
                if (inlineDetail.isNotBlank()) {
                    Spacer(Modifier.width(8.dp))
                    Text(inlineDetail, style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSecondaryContainer.copy(alpha = 0.6f),
                        maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                } else Spacer(Modifier.weight(1f))
                if (hasBody) Icon(
                    if (expanded) Icons.Filled.KeyboardArrowDown else Icons.AutoMirrored.Filled.KeyboardArrowRight,
                    contentDescription = null, modifier = Modifier.size(16.dp),
                    tint = MaterialTheme.colorScheme.outline)
            }
        }
        if (hasBody) AnimatedVisibility(expanded) {
            Surface(color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.4f),
                shape = RoundedCornerShape(10.dp),
                modifier = Modifier.padding(start = 16.dp, top = 3.dp, bottom = 2.dp).fillMaxWidth()) {
                Column(Modifier.padding(horizontal = 10.dp, vertical = 6.dp)) {
                    val mono = MaterialTheme.typography.bodySmall.copy(
                        fontFamily = androidx.compose.ui.text.font.FontFamily.Monospace)
                    if (detail.isNotBlank()) {
                        Text("input", style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary)
                        Text(detail, style = mono, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    if (m.output.isNotBlank()) {
                        if (detail.isNotBlank()) Spacer(Modifier.height(6.dp))
                        Text("output", style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.primary)
                        // Cap the rendered output — tool results can be enormous.
                        Text(m.output.take(4000), style = mono,
                            color = MaterialTheme.colorScheme.onSurfaceVariant)
                        if (m.output.length > 4000) Text("… (${m.output.length} chars total)",
                            style = MaterialTheme.typography.labelSmall,
                            color = MaterialTheme.colorScheme.outline)
                    }
                }
            }
        }
    }
}

@Composable
private fun ErrorBubble(text: String) {
    Surface(color = MaterialTheme.colorScheme.errorContainer,
        contentColor = MaterialTheme.colorScheme.onErrorContainer, shape = RoundedCornerShape(8.dp),
        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
        Text(text, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(10.dp))
    }
}

@Composable
internal fun TypingIndicator() {
    // Text-only: the spinner read as a stuck progress bar and the user wanted
    // the thinking message alone while a turn runs. The loading indicator is
    // reserved for opening a session and waiting for its replay.
    Text(stringResource(R.string.grouse_is_thinking), style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.outline,
        modifier = Modifier.padding(horizontal = 4.dp, vertical = 2.dp))
}

// ---- Schedules and recipes ------------------------------------------------------------------
//
// These two screens are one feature seen from both ends. A SCHEDULE is a cron entry; a RECIPE is
// what it runs. goose keeps them separately and links them only by file path, so the list screen
// shows jobs and the detail screen edits the recipe behind one -- editing a recipe changes what
// the job does on its next run, with nothing to re-register.
//
// Goose Desktop deliberately does less than this: its schedule detail view prints the recipe's
// PATH and stops, because Desktop can open the file with a native picker and Grouse cannot. A
// phone has no filesystem in common with the server, which is why the recipe library API
// (recipes/list returns whole recipes) is the only workable way to show any of this here.
