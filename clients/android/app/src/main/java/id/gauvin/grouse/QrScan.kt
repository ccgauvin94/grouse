// SPDX-License-Identifier: AGPL-3.0-or-later

package id.gauvin.grouse

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


/** Camera QR scan for roam pairing. Decodes a `goose+roam://` connection card —
 *  the same string the paste field takes — and hands it to [onResult] once. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun QrScanScreen(onResult: (String) -> Unit, onCancel: () -> Unit) {
    val ctx = LocalContext.current
    var granted by remember {
        mutableStateOf(androidx.core.content.ContextCompat.checkSelfPermission(
            ctx, android.Manifest.permission.CAMERA) == android.content.pm.PackageManager.PERMISSION_GRANTED)
    }
    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()) { ok -> granted = ok }
    Scaffold(topBar = {
        TopAppBar(title = { Text(stringResource(R.string.scan_host_card)) },
            navigationIcon = {
                IconButton(onClick = onCancel) {
                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "back")
                }
            })
    }) { pad ->
        if (!granted) {
            Column(Modifier.padding(pad).padding(24.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp)) {
                Text(stringResource(R.string.camera_access_note))
                Button(onClick = { launcher.launch(android.Manifest.permission.CAMERA) }) {
                    Text(stringResource(R.string.allow_camera))
                }
            }
        } else {
            CameraQrPreview(onCard = onResult, modifier = Modifier.padding(pad).fillMaxSize())
        }
    }
}

/** Renders a QR of this device's roam identity so a HOST can scan the phone to pair
 *  (the reverse of "Scan QR", which decodes a host's card). Encodes the same public key
 *  the host would see in `peers list` and would `roam peers accept`. */
@Composable
fun DeviceQr(cm: ConnectionManager, modifier: Modifier = Modifier) {
    val key = remember(cm.roamPublicKey) { cm.roamPublicKey }
    val qr = remember(key) {
        runCatching {
            val matrix = com.google.zxing.qrcode.QRCodeWriter().encode(
                key, com.google.zxing.BarcodeFormat.QR_CODE, 256, 256)
            val px = IntArray(matrix.width * matrix.height)
            for (y in 0 until matrix.height) {
                for (x in 0 until matrix.width) {
                    px[y * matrix.width + x] = if (matrix[x, y])
                        android.graphics.Color.BLACK else android.graphics.Color.WHITE
                }
            }
            android.graphics.Bitmap.createBitmap(matrix.width, matrix.height,
                android.graphics.Bitmap.Config.ARGB_8888).apply {
                setPixels(px, 0, matrix.width, 0, 0, matrix.width, matrix.height)
            }.asImageBitmap()
        }.getOrNull()
    }
    if (qr != null) {
        Image(bitmap = qr, contentDescription = "This device's roam pairing QR",
            modifier = modifier)
    } else {
        Text(stringResource(R.string.couldnt_render_qr), style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error)
    }
}

/** CameraX preview + ML Kit QR decode; fires [onCard] once with the raw value. */
@Composable
private fun CameraQrPreview(onCard: (String) -> Unit, modifier: Modifier = Modifier) {
    val ctx = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val mainExecutor = androidx.core.content.ContextCompat.getMainExecutor(ctx)
    val fired = remember { java.util.concurrent.atomic.AtomicBoolean(false) }
    val previewView = remember { androidx.camera.view.PreviewView(ctx) }
    DisposableEffect(Unit) {
        val providerFuture = androidx.camera.lifecycle.ProcessCameraProvider.getInstance(ctx)
        val bind = Runnable {
            runCatching {
                val provider = providerFuture.get()
                val preview = androidx.camera.core.Preview.Builder().build().also {
                    it.setSurfaceProvider(previewView.surfaceProvider)
                }
                val analysis = androidx.camera.core.ImageAnalysis.Builder()
                    .setBackpressureStrategy(androidx.camera.core.ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(mainExecutor) { imageProxy ->
                    val media = imageProxy.image
                    if (media == null) { imageProxy.close(); return@setAnalyzer }
                    try {
                        // The Y (luminance) plane is all zxing needs; QR codes
                        // decode at any rotation, so sensor orientation is irrelevant.
                        val buffer = media.planes[0].buffer
                        val data = ByteArray(buffer.remaining())
                        buffer.get(data)
                        val source = com.google.zxing.PlanarYUVLuminanceSource(
                            data, imageProxy.width, imageProxy.height,
                            0, 0, imageProxy.width, imageProxy.height, false)
                        val result = try {
                            com.google.zxing.qrcode.QRCodeReader().decode(
                                com.google.zxing.BinaryBitmap(
                                    com.google.zxing.common.HybridBinarizer(source)))
                        } catch (_: Exception) { null }
                        if (result != null && fired.compareAndSet(false, true)) onCard(result.text)
                    } finally {
                        imageProxy.close()
                    }
                }
                provider.unbindAll()
                provider.bindToLifecycle(lifecycleOwner, androidx.camera.core.CameraSelector.DEFAULT_BACK_CAMERA,
                    preview, analysis)
            }
        }
        providerFuture.addListener(bind, mainExecutor)
        onDispose {
            // Release the camera when leaving the screen (the provider is process-scoped).
            providerFuture.addListener(
                { runCatching { providerFuture.get().unbindAll() } }, mainExecutor)
        }
    }
    AndroidView(factory = { previewView }, modifier = modifier)
}
