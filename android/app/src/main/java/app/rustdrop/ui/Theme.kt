package app.rustdrop.ui

import android.content.Context
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Shapes
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp
import androidx.core.content.edit
import org.json.JSONObject

/** Values from the shared/design JSON files (packaged as assets/design). Components never hardcode colours. */
@Immutable
data class Palette(
    val background: Color,
    val surface: Color,
    val surfaceElevated: Color,
    val primary: Color,
    val primaryButton: Color,
    val onPrimary: Color,
    val success: Color,
    val warning: Color,
    val error: Color,
    val textPrimary: Color,
    val textSecondary: Color,
    val border: Color,
)

@Immutable
data class Spacing(val xs: Dp, val sm: Dp, val md: Dp, val lg: Dp, val xl: Dp, val xxl: Dp, val xxxl: Dp)

@Immutable
data class Radius(val badge: Dp, val input: Dp, val button: Dp, val card: Dp, val panel: Dp, val dialog: Dp)

class Tokens(val dark: Palette, val light: Palette, val spacing: Spacing, val radius: Radius, val type: Typography)

enum class ThemePreference { System, Dark, Light }

val LocalPalette = staticCompositionLocalOf<Palette> { error("RustDropTheme not applied") }
val LocalSpacing = staticCompositionLocalOf<Spacing> { error("RustDropTheme not applied") }
val LocalRadius = staticCompositionLocalOf<Radius> { error("RustDropTheme not applied") }
val LocalThemePreference = staticCompositionLocalOf<(ThemePreference) -> Unit> { {} }
val LocalCurrentTheme = staticCompositionLocalOf { ThemePreference.System }

/** The transfer island is always dark, like a hardware cut-out, whatever the theme. */
val LocalIsland = staticCompositionLocalOf<Palette> { error("RustDropTheme not applied") }

private fun json(ctx: Context, name: String) =
    JSONObject(ctx.assets.open("design/$name.json").bufferedReader().use { it.readText() })

private fun JSONObject.color(key: String) = Color(android.graphics.Color.parseColor(getString(key)))

private fun palette(o: JSONObject) = Palette(
    background = o.color("background"),
    surface = o.color("surface"),
    surfaceElevated = o.color("surfaceElevated"),
    primary = o.color("primary"),
    primaryButton = o.color("primaryButton"),
    onPrimary = o.color("onPrimary"),
    success = o.color("success"),
    warning = o.color("warning"),
    error = o.color("error"),
    textPrimary = o.color("textPrimary"),
    textSecondary = o.color("textSecondary"),
    border = o.color("border"),
)

fun loadTokens(ctx: Context): Tokens {
    val colors = json(ctx, "colors")
    val s = json(ctx, "spacing")
    val r = json(ctx, "radius")
    val scale = json(ctx, "typography").getJSONObject("scale")
    fun d(o: JSONObject, k: String) = o.getInt(k).dp
    fun style(k: String): TextStyle {
        val t = scale.getJSONObject(k)
        return TextStyle(
            fontSize = t.getInt("size").sp,
            fontWeight = FontWeight(t.getInt("weight")),
            lineHeight = t.getDouble("lineHeight").em,
        )
    }
    return Tokens(
        dark = palette(colors.getJSONObject("dark")),
        light = palette(colors.getJSONObject("light")),
        spacing = Spacing(d(s, "xs"), d(s, "sm"), d(s, "md"), d(s, "lg"), d(s, "xl"), d(s, "2xl"), d(s, "3xl")),
        radius = Radius(d(r, "badge"), d(r, "input"), d(r, "button"), d(r, "card"), d(r, "panel"), d(r, "dialog")),
        type = Typography(
            displaySmall = style("display"),
            headlineMedium = style("h1"),
            headlineSmall = style("h2"),
            titleLarge = style("h3"),
            titleMedium = style("bodyLarge").copy(fontWeight = FontWeight.SemiBold),
            bodyLarge = style("bodyLarge"),
            bodyMedium = style("body"),
            bodySmall = style("caption"),
            labelLarge = style("body").copy(fontWeight = FontWeight.SemiBold),
            labelSmall = style("smallLabel"),
        ),
    )
}

@Composable
fun RustDropTheme(content: @Composable () -> Unit) {
    val ctx = LocalContext.current
    val tokens = remember { loadTokens(ctx) }
    val prefs = remember { ctx.getSharedPreferences("ui", Context.MODE_PRIVATE) }
    var preference by remember {
        mutableStateOf(runCatching { ThemePreference.valueOf(prefs.getString("theme", null)!!) }.getOrDefault(ThemePreference.System))
    }
    val dark = when (preference) {
        ThemePreference.System -> isSystemInDarkTheme()
        ThemePreference.Dark -> true
        ThemePreference.Light -> false
    }
    val p = if (dark) tokens.dark else tokens.light
    // Filled buttons use primaryButton: white on the brand blue is below 4.5:1 contrast.
    val base = if (dark) darkColorScheme() else lightColorScheme()
    val scheme = base.copy(
        primary = p.primaryButton,
        onPrimary = p.onPrimary,
        secondaryContainer = p.surfaceElevated,
        onSecondaryContainer = p.textPrimary,
        background = p.background,
        onBackground = p.textPrimary,
        surface = p.background,
        onSurface = p.textPrimary,
        surfaceVariant = p.surfaceElevated,
        onSurfaceVariant = p.textSecondary,
        surfaceContainerLowest = p.background,
        surfaceContainerLow = p.surface,
        surfaceContainer = p.surface,
        surfaceContainerHigh = p.surfaceElevated,
        surfaceContainerHighest = p.surfaceElevated,
        outline = p.border,
        outlineVariant = p.border,
        error = p.error,
    )
    val r = tokens.radius
    MaterialTheme(
        colorScheme = scheme,
        typography = tokens.type,
        shapes = Shapes(
            small = RoundedCornerShape(r.badge),
            medium = RoundedCornerShape(r.button),
            large = RoundedCornerShape(r.card),
            extraLarge = RoundedCornerShape(r.dialog),
        ),
    ) {
        CompositionLocalProvider(
            LocalPalette provides p,
            LocalSpacing provides tokens.spacing,
            LocalRadius provides tokens.radius,
            LocalCurrentTheme provides preference,
            LocalIsland provides tokens.dark,
            LocalThemePreference provides { next ->
                preference = next
                prefs.edit { putString("theme", next.name) }
            },
            content = content,
        )
    }
}
