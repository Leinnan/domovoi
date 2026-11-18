package com.crayen.domovoi.ui.pairing

import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.FilledTonalIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.activity.compose.BackHandler
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.crayen.domovoi.ui.theme.Space
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors

/** The camera, looking for a `domovoi://connect` QR code (ML Kit, on device). */
@Composable
fun QrScannerScreen(onFound: (String) -> Unit, onClose: () -> Unit) {
    val context = LocalContext.current
    val lifecycle = LocalLifecycleOwner.current
    var wrongCode by remember { mutableStateOf(false) }
    var done by remember { mutableStateOf(false) }
    BackHandler(onBack = onClose)

    val previewView = remember { PreviewView(context).apply { scaleType = PreviewView.ScaleType.FILL_CENTER } }
    DisposableEffect(lifecycle) {
        val executor = Executors.newSingleThreadExecutor()
        val scanner = BarcodeScanning.getClient(BarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).build())
        val providerFuture = ProcessCameraProvider.getInstance(context)
        providerFuture.addListener({
            val provider = providerFuture.get()
            val preview = Preview.Builder().build().also { it.surfaceProvider = previewView.surfaceProvider }
            val analysis = ImageAnalysis.Builder().setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST).build()
            analysis.setAnalyzer(executor) { proxy ->
                val media = proxy.image
                if (media == null || done) { proxy.close(); return@setAnalyzer }
                scanner.process(InputImage.fromMediaImage(media, proxy.imageInfo.rotationDegrees))
                    .addOnSuccessListener { codes ->
                        val value = codes.firstNotNullOfOrNull { it.rawValue }
                        when {
                            value == null -> Unit
                            value.startsWith("domovoi://") && !done -> { done = true; onFound(value) }
                            else -> wrongCode = true
                        }
                    }
                    .addOnCompleteListener { proxy.close() }
            }
            provider.unbindAll()
            provider.bindToLifecycle(lifecycle, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
        }, ContextCompat.getMainExecutor(context))
        onDispose {
            runCatching { providerFuture.get().unbindAll() }
            scanner.close()
            executor.shutdown()
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        AndroidView(factory = { previewView }, modifier = Modifier.fillMaxSize())
        Box(
            Modifier.align(Alignment.Center).size(260.dp)
                .border(BorderStroke(3.dp, Color.White.copy(alpha = 0.9f)), RoundedCornerShape(28.dp)),
        )
        FilledTonalIconButton(onClick = onClose, modifier = Modifier.safeDrawingPadding().padding(Space.l)) {
            Icon(Icons.Filled.Close, contentDescription = "Close")
        }
        Column(
            Modifier.align(Alignment.BottomCenter).safeDrawingPadding().padding(Space.xl).fillMaxWidth()
                .background(Color.Black.copy(alpha = 0.55f), RoundedCornerShape(20.dp)).padding(Space.l),
        ) {
            Text(
                if (wrongCode) "That code isn't a Domovoi pairing code." else "Point the camera at the pairing code Domovoi Server shows.",
                color = Color.White,
                style = MaterialTheme.typography.bodyLarge,
                textAlign = TextAlign.Center,
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}
