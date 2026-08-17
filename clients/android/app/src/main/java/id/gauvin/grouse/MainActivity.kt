@file:OptIn(ExperimentalMaterial3Api::class)

package id.gauvin.grouse

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.compose.ui.graphics.Color
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.core.content.IntentCompat
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.MenuBook
import androidx.compose.material.icons.filled.Psychology
import androidx.compose.material.icons.filled.School
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Button
import androidx.compose.material3.DrawerValue
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalDrawerSheet
import androidx.compose.material3.ModalNavigationDrawer
import androidx.compose.material3.NavigationDrawerItem
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.rememberDrawerState
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import id.gauvin.grouse.ui.theme.GooseTheme
import kotlinx.coroutines.launch

class MainActivity : FragmentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val cm = ConnectionManager.get(this)
        handleEntry(intent, cm)
        setContent {
            GooseTheme(dynamicColor = cm.dynamicColor.value) {
                AppRoot(this@MainActivity, cm)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleEntry(intent, ConnectionManager.get(this))
    }

    /** Route share-sheet / shortcut / tile intents into ConnectionManager for the UI to pick up. */
    private fun handleEntry(intent: Intent?, cm: ConnectionManager) {
        when (intent?.action) {
            Intent.ACTION_SEND -> {
                intent.getStringExtra(Intent.EXTRA_TEXT)?.let { cm.pendingShareText.value = it }
                IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                    ?.let { readImage(this, it)?.let(cm.pendingShareImages::add) }
            }
            Intent.ACTION_SEND_MULTIPLE ->
                IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                    ?.forEach { readImage(this, it)?.let(cm.pendingShareImages::add) }
            ACTION_NEW_CHAT -> cm.pendingNewChat.value = true
            ACTION_OPEN_SESSION ->
                intent.getStringExtra(EXTRA_SESSION_ID)?.let { cm.pendingOpenSession.value = it }
        }
    }

    companion object {
        const val ACTION_NEW_CHAT = "id.gauvin.grouse.NEW_CHAT"
        const val ACTION_OPEN_SESSION = "id.gauvin.grouse.OPEN_SESSION"
        const val EXTRA_SESSION_ID = "id.gauvin.grouse.extra.SESSION_ID"
    }
}

@Composable
fun AppRoot(activity: FragmentActivity, cm: ConnectionManager) {
    // Lock the app behind biometrics only when the user has opted in (Settings › Security,
    // default off) AND a key is stored AND an authenticator is enrolled.
    val needsLock = remember { cm.store.biometricLock && cm.configured && Biometric.available(activity) }
    // Plain `remember` (NOT rememberSaveable): a saved `unlocked=true` would survive process death
    // and let the app reopen without a prompt. Any recreation must re-lock.
    var unlocked by remember { mutableStateOf(!needsLock) }
    var error by remember { mutableStateOf<String?>(null) }
    // True while a prompt is on screen, so a device-credential screen (which stops our activity)
    // doesn't trigger a re-lock / re-prompt loop.
    var authenticating by remember { mutableStateOf(false) }

    fun authenticate() {
        if (authenticating) return
        authenticating = true
        Biometric.prompt(
            activity,
            onSuccess = { authenticating = false; unlocked = true; error = null },
            onFail = { authenticating = false; error = it },
        )
    }

    // Re-lock when backgrounded; (re)prompt when foregrounded while locked. On observer
    // registration the Lifecycle replays up to the current state, so this fires the initial
    // cold-start prompt too.
    val lockOwner = LocalLifecycleOwner.current
    DisposableEffect(lockOwner, needsLock) {
        val obs = LifecycleEventObserver { _, e ->
            if (!needsLock) return@LifecycleEventObserver
            when (e) {
                Lifecycle.Event.ON_STOP -> if (!authenticating) unlocked = false
                Lifecycle.Event.ON_RESUME -> if (!unlocked && !authenticating) authenticate()
                else -> {}
            }
        }
        lockOwner.lifecycle.addObserver(obs)
        onDispose { lockOwner.lifecycle.removeObserver(obs) }
    }

    // The main tree stays composed while locked — the lock screen is an opaque overlay on top.
    // An early return here used to tear the whole UI down on every lock, so unlocking rebuilt
    // the NavHost from scratch: back to the chat route, Settings position and scroll state gone.
    Box(Modifier.fillMaxSize()) {
        MainApp(activity, cm, unlocked)
        if (!unlocked) LockScreen(error) { authenticate() }
    }
}

@Composable
private fun MainApp(activity: FragmentActivity, cm: ConnectionManager, unlocked: Boolean) {    // Ask for notification permission so backgrounded turns can alert (API 33+). Gated on
    // unlocked so the dialog doesn't compete with the biometric prompt at cold start.
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        val notifPerm = rememberLauncherForActivityResult(
            ActivityResultContracts.RequestPermission()) {}
        LaunchedEffect(unlocked) { if (unlocked) notifPerm.launch(Manifest.permission.POST_NOTIFICATIONS) }
    }

    // Ask for mic up front: push-to-talk needs it, and the assistant VoiceInteractionSession
    // can't request runtime permissions itself — so the app must obtain it through the Activity.
    val micPerm = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}
    LaunchedEffect(unlocked) {
        if (unlocked && ContextCompat.checkSelfPermission(activity, Manifest.permission.RECORD_AUDIO)
            != PackageManager.PERMISSION_GRANTED) {
            micPerm.launch(Manifest.permission.RECORD_AUDIO)
        }
    }

    val nav = rememberNavController()
    // Fresh start lands on the Assistant thread; re-entry after the lock screen (or any
    // recreation) only reconnects to whatever session was already open.
    LaunchedEffect(Unit) { cm.connectHome() }
    // "New chat" from a shortcut/tile: start fresh and land on the chat screen.
    LaunchedEffect(cm.pendingNewChat.value) {
        if (cm.pendingNewChat.value && cm.configured) {
            cm.pendingNewChat.value = false
            cm.newSession()
            nav.navigate("chat") { popUpTo("chat") { inclusive = true } }
        }
    }
    // A finished-turn notification tap: open that session and land on the chat screen.
    LaunchedEffect(cm.pendingOpenSession.value) {
        cm.pendingOpenSession.value?.let { sid ->
            if (cm.configured) {
                cm.pendingOpenSession.value = null
                cm.openSession(sid)
                nav.navigate("chat") { popUpTo("chat") { inclusive = true } }
            }
        }
    }

    val drawerState = rememberDrawerState(DrawerValue.Closed)
    val drawerScope = rememberCoroutineScope()
    val backStack by nav.currentBackStackEntryAsState()
    val route = backStack?.destination?.route
    fun closeDrawer() = drawerScope.launch { drawerState.close() }
    fun openDrawer() = drawerScope.launch { drawerState.open() }

    ModalNavigationDrawer(
        drawerState = drawerState,
        // No drawer on the onboarding screen, and not once it's mid-swipe-away from "connect" either
        // -- gate strictly on being past onboarding.
        gesturesEnabled = cm.configured && route != "connect",
        drawerContent = {
            ModalDrawerSheet {
                // Re-fetch the session list whenever the menu opens — it IS the chats list now,
                // so it must reflect renames/archives/new sessions from any client.
                LaunchedEffect(drawerState.isOpen) { if (drawerState.isOpen) cm.refreshSidebar() }
                Column(Modifier.fillMaxHeight().padding(vertical = 12.dp)) {
                    // Main | Roam — the chats world vs. peer management. Roam replaces the old
                    // standalone RoamScreen route entirely.
                    var drawerTab by rememberSaveable { mutableStateOf("main") }
                    TabRow(selectedTabIndex = if (drawerTab == "roam") 1 else 0,
                        modifier = Modifier.padding(horizontal = 12.dp)) {
                        Tab(selected = drawerTab == "main",
                            onClick = { drawerTab = "main" },
                            text = { Text("Main") })
                        Tab(selected = drawerTab == "roam",
                            onClick = { drawerTab = "roam" },
                            text = { Text("Roam") })
                    }
                    if (drawerTab == "main") {
                        if (cm.assistantEnabled.value) {
                            NavigationDrawerItem(
                                label = { Text("Assistant") },
                                icon = { Icon(Icons.Filled.Psychology, contentDescription = null) },
                                selected = route == "chat" && cm.onAssistant,
                                onClick = {
                                    closeDrawer(); cm.openAssistant()
                                    nav.navigate("chat") { launchSingleTop = true; popUpTo("chat") { inclusive = false } }
                                },
                                modifier = Modifier.padding(horizontal = 12.dp),
                            )
                            HorizontalDivider(Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
                        }
                        // The whole chats world lives in the menu: projects (collapsible) then free
                        // chats. Tap opens; long-press renames/archives. Scrolls independently so
                        // Settings stays pinned at the bottom.
                        Box(Modifier.weight(1f)) {
                            DrawerChats(cm, onOpen = {
                                closeDrawer()
                                nav.navigate("chat") { launchSingleTop = true; popUpTo("chat") { inclusive = true } }
                            }, onOpenProject = { p ->
                                closeDrawer()
                                nav.navigate("project/" + Uri.encode(p)) { launchSingleTop = true }
                            })
                        }
                        HorizontalDivider(Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
                        // What the agent can be given, and when it runs: skills are the notes it
                        // pulls in on demand, recipes are the jobs, the scheduler is their cron.
                        // All three are server state that was previously only reachable by editing
                        // files on the box. Compact rows so the chats list keeps the space.
                        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceEvenly) {
                            CompactNavItem("Skills", Icons.Filled.School, route == "skills") {
                                closeDrawer(); nav.navigate("skills") { launchSingleTop = true }
                            }
                            CompactNavItem("Recipes", Icons.Filled.MenuBook, route == "recipes") {
                                closeDrawer(); nav.navigate("recipes") { launchSingleTop = true }
                            }
                            CompactNavItem("Settings", Icons.Filled.Settings, route == "settings") {
                                closeDrawer(); nav.navigate("settings") { launchSingleTop = true }
                            }
                        }
                    } else {
                        Column(Modifier.fillMaxWidth()) {
                            Box(Modifier.weight(1f).fillMaxWidth()) {
                                RoamBrowse(cm, nav, onOpen = {
                                    closeDrawer()
                                    nav.navigate("chat") { launchSingleTop = true; popUpTo("chat") { inclusive = true } }
                                })
                            }
                            HorizontalDivider(Modifier.padding(horizontal = 16.dp))
                            // Add/management is a full-screen page (own route) — it carries the
                            // camera QR scanner, which must not sit under the drawer scrim.
                            NavigationDrawerItem(
                                label = { Text("New connection") },
                                icon = { Icon(Icons.Filled.Add, contentDescription = null) },
                                selected = route == "roam_add",
                                onClick = { closeDrawer(); nav.navigate("roam_add") { launchSingleTop = true } },
                                modifier = Modifier.padding(horizontal = 12.dp),
                            )
                        }
                    }
                }
            }
        },
    ) {
        Box(Modifier.fillMaxSize()) {
            NavHost(nav, startDestination = if (cm.configured) "landing" else "connect") {
                composable("connect") {
                    ConnectScreen(cm) { nav.navigate("chat") { popUpTo("connect") { inclusive = true } } }
            }
            composable("chat") { ChatScreen(cm, onOpenDrawer = ::openDrawer) }
            composable("assistant_settings") { AssistantSettingsScreen(cm, nav) }
            composable("project/{pname}") { back ->
                val pname = Uri.decode(back.arguments?.getString("pname") ?: "")
                ProjectScreen(cm, nav, pname)
            }
            composable("settings") { SettingsScreen(cm, nav, onOpenDrawer = ::openDrawer) }
            composable("roam_add") { RoamAddConnectionScreen(cm, nav) }
            composable("qrscan") {
                QrScanScreen(
                    onResult = { card ->
                        // The scanner's previous entry is the standalone add page.
                        nav.previousBackStackEntry?.savedStateHandle?.set("qr_card", card)
                        nav.popBackStack()
                    },
                    onCancel = { nav.popBackStack() },
                )
            }
            composable("extensions") { ExtensionsScreen(cm, nav) }
            composable("instance") { InstanceScreen(cm, nav) }
            composable("providers") { ProvidersScreen(cm, nav) }
            composable("recipes") {
                RecipesScreen(cm, nav, onOpenChat = {
                    nav.navigate("chat") { launchSingleTop = true; popUpTo("chat") { inclusive = true } }
                })
            }
            composable("skills") { SkillsScreen(cm, nav) }
            composable("skill/{name}") { back ->
                SkillScreen(cm, nav, Uri.decode(back.arguments?.getString("name") ?: ""))
            }
            composable("recipe/{rid}") { back ->
                RecipeScreen(cm, nav, back.arguments?.getString("rid") ?: "", onOpenChat = {
                    nav.navigate("chat") { launchSingleTop = true; popUpTo("chat") { inclusive = true } }
                })
            }
            composable("landing") { LandingScreen(cm, onOpenDrawer = ::openDrawer, onNewChat = {
                cm.newSession()
                nav.navigate("chat") { launchSingleTop = true; popUpTo("landing") { inclusive = true } }
            }, onOpenAssistant = {
                cm.openAssistant()
                nav.navigate("chat") { launchSingleTop = true; popUpTo("landing") { inclusive = true } }
            }, onOpenSession = { sid ->
                cm.openSession(sid)
                nav.navigate("chat") { launchSingleTop = true; popUpTo("landing") { inclusive = true } }
            }) }
        }
        // System-wide activity: a turn in flight (or a replay/connect) squeezes a
        // thin bar under the status bar so any screen telegraphs that work is
        // happening, not just the chat header.
        if (cm.busy.value) {
            LinearProgressIndicator(
                modifier = Modifier.fillMaxWidth().height(2.dp).align(Alignment.TopCenter),
            )
        }
        }
    }
}

@Composable
fun LockScreen(error: String?, onUnlock: () -> Unit) {
    // Drawn as an overlay above the still-composed app: must be opaque and swallow every
    // pointer event so nothing shows or scrolls through.
    Box(
        Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) awaitPointerEvent().changes.forEach { it.consume() }
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Icon(Icons.Filled.Lock, contentDescription = null, modifier = Modifier.size(48.dp),
                tint = MaterialTheme.colorScheme.primary)
            Spacer(Modifier.height(10.dp))
            Text("Grouse is locked", style = MaterialTheme.typography.titleMedium)
            if (error != null) {
                Spacer(Modifier.height(4.dp))
                Text(error, style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error)
            }
            Spacer(Modifier.height(14.dp))
            Button(onClick = onUnlock) { Text("Unlock") }
        }
    }
}

/** Compact bottom-nav row for the drawer (Skills/Recipes/Settings) — smaller than
 *  a full NavigationDrawerItem so more drawer height goes to the chats list. */
@Composable
private fun CompactNavItem(
    label: String,
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    selected: Boolean,
    onClick: () -> Unit,
) {
    val shape = RoundedCornerShape(8.dp)
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .clip(shape)
            .background(
                if (selected) MaterialTheme.colorScheme.secondaryContainer
                else Color.Transparent
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 6.dp),
    ) {
        Icon(icon, contentDescription = null, modifier = Modifier.size(16.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.width(4.dp))
        Text(label, style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}
