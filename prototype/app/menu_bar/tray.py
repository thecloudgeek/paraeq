"""System tray icon with profile switching, bypass toggle, and quick access."""
import logging

from PyQt6.QtCore import Qt
from PyQt6.QtGui import QAction, QColor, QIcon, QPixmap
from PyQt6.QtWidgets import QApplication, QMenu, QSystemTrayIcon

logger = logging.getLogger(__name__)


def _make_default_icon(color: str = "#4FC3F7") -> QIcon:
    """Generate a simple coloured square icon when no image asset is available."""
    pixmap = QPixmap(32, 32)
    pixmap.fill(QColor(color))
    return QIcon(pixmap)


class SystemTray(QSystemTrayIcon):
    """System-tray icon for ParaEQ with context menu."""

    def __init__(self, app: QApplication, main_window, parent=None):
        super().__init__(parent)
        self._app = app
        self._main_window = main_window

        self._bypassed: bool = False
        self._profiles: list[str] = []
        self._active_profile: str | None = None

        # Callbacks (optional, set via set_callbacks)
        self._on_bypass_change = None
        self._on_profile_select = None

        self.setIcon(_make_default_icon())
        self.setToolTip("ParaEQ")
        self._build_menu()
        self.activated.connect(self._on_activated)
        logger.info("SystemTray initialised")

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def show(self):
        """Show the tray icon (only if the platform supports it)."""
        if QSystemTrayIcon.isSystemTrayAvailable():
            super().show()
            logger.info("System tray icon visible")
        else:
            logger.warning("System tray not available on this platform")

    def set_callbacks(
        self,
        on_bypass_change=None,
        on_profile_select=None,
    ):
        """Register optional callbacks for bypass toggle and profile switching."""
        self._on_bypass_change = on_bypass_change
        self._on_profile_select = on_profile_select

    def update_profiles(self, profile_names: list[str], active: str | None = None):
        """Refresh the profile submenu with the given names."""
        self._profiles = list(profile_names)
        self._active_profile = active
        self._rebuild_profiles_menu()
        logger.debug("SystemTray: updated %d profiles", len(profile_names))

    def set_active_profile(self, name: str | None):
        """Update the displayed active profile name."""
        self._active_profile = name
        label = name or "None"
        self._profile_label_action.setText(f"Profile: {label}")
        self._rebuild_profiles_menu()

    def set_bypassed(self, bypassed: bool):
        """Sync the bypass toggle state."""
        self._bypassed = bypassed
        self._bypass_action.setChecked(bypassed)
        self.setToolTip(f"ParaEQ {'(bypassed)' if bypassed else ''}")

    # ------------------------------------------------------------------
    # Internal
    # ------------------------------------------------------------------

    def _build_menu(self):
        menu = QMenu()

        # Profile display
        self._profile_label_action = QAction("Profile: None", self)
        self._profile_label_action.setEnabled(False)
        menu.addAction(self._profile_label_action)

        menu.addSeparator()

        # Profile sub-menu placeholder
        self._profiles_menu = QMenu("Switch Profile")
        menu.addMenu(self._profiles_menu)

        menu.addSeparator()

        # Bypass toggle
        self._bypass_action = QAction("Bypass EQ", self)
        self._bypass_action.setCheckable(True)
        self._bypass_action.setChecked(False)
        self._bypass_action.toggled.connect(self._on_bypass_toggled)
        menu.addAction(self._bypass_action)

        menu.addSeparator()

        # Show window
        show_action = QAction("Show Window", self)
        show_action.triggered.connect(self._show_window)
        menu.addAction(show_action)

        # Quit
        quit_action = QAction("Quit ParaEQ", self)
        quit_action.triggered.connect(self._quit)
        menu.addAction(quit_action)

        self.setContextMenu(menu)
        self._menu = menu

    def _rebuild_profiles_menu(self):
        self._profiles_menu.clear()
        if not self._profiles:
            no_profiles = QAction("(no profiles)", self)
            no_profiles.setEnabled(False)
            self._profiles_menu.addAction(no_profiles)
            return

        for name in self._profiles:
            action = QAction(name, self)
            action.setCheckable(True)
            action.setChecked(name == self._active_profile)
            action.triggered.connect(lambda checked, n=name: self._on_profile_action(n))
            self._profiles_menu.addAction(action)

    def _on_bypass_toggled(self, checked: bool):
        self._bypassed = checked
        self.setToolTip(f"ParaEQ {'(bypassed)' if checked else ''}")
        logger.info("Bypass toggled: %s", checked)
        if self._on_bypass_change is not None:
            self._on_bypass_change(checked)

    def _on_profile_action(self, name: str):
        self._active_profile = name
        self._profile_label_action.setText(f"Profile: {name}")
        self._rebuild_profiles_menu()
        logger.info("Profile selected from tray: %s", name)
        if self._on_profile_select is not None:
            self._on_profile_select(name)

    def _show_window(self):
        window = self._main_window
        window.showNormal()
        window.raise_()
        window.activateWindow()

    def _quit(self):
        logger.info("Quit requested from tray")
        self._app.quit()

    def _on_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.DoubleClick:
            self._show_window()
