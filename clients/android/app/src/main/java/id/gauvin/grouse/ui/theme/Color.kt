package id.gauvin.grouse.ui.theme

import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.ui.graphics.Color

// Fallback palette for API < 31 (no Material You). A muted goose-green so the app still
// has an identity when it can't pull dynamic color from the wallpaper.
private val Green40 = Color(0xFF4C662B)
private val Green80 = Color(0xFFB1D18A)
private val GreenContainer = Color(0xFFCDEDA3)
private val Olive40 = Color(0xFF586249)
private val Olive80 = Color(0xFFBFCBAD)

val FallbackLight = lightColorScheme(
    primary = Green40,
    onPrimary = Color.White,
    primaryContainer = GreenContainer,
    onPrimaryContainer = Color(0xFF102000),
    secondary = Olive40,
    secondaryContainer = Color(0xFFDCE7C8),
    onSecondaryContainer = Color(0xFF151E0B),
)

val FallbackDark = darkColorScheme(
    primary = Green80,
    onPrimary = Color(0xFF1F3701),
    primaryContainer = Color(0xFF354E16),
    onPrimaryContainer = GreenContainer,
    secondary = Olive80,
    secondaryContainer = Color(0xFF404A33),
    onSecondaryContainer = Color(0xFFDCE7C8),
)
