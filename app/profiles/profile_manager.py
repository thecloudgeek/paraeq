"""Profile manager GUI: list, activate, duplicate, rename, delete, import/export."""
import logging
from pathlib import Path

from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QFileDialog,
    QGroupBox,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QMessageBox,
    QPushButton,
    QVBoxLayout,
    QWidget,
)

from paraeq.profiles.profile import Profile, ProfileManager

logger = logging.getLogger(__name__)


class ProfileManagerWidget(QWidget):
    """Panel for managing headphone correction profiles."""

    profile_activated = pyqtSignal(object)  # Profile

    def __init__(self, parent=None):
        super().__init__(parent)
        self._profiles: list[Profile] = []
        self._profile_manager = ProfileManager()
        self._setup_ui()

    # ------------------------------------------------------------------
    # UI
    # ------------------------------------------------------------------

    def _setup_ui(self):
        root = QVBoxLayout(self)
        root.setSpacing(6)

        # --- Profile list ---
        list_group = QGroupBox("Saved Profiles")
        list_layout = QVBoxLayout(list_group)
        self._list = QListWidget()
        self._list.itemDoubleClicked.connect(self._activate_selected)
        list_layout.addWidget(self._list)
        root.addWidget(list_group)

        # --- Action buttons ---
        btn_group = QGroupBox("Actions")
        btn_layout = QHBoxLayout(btn_group)

        self._activate_btn = QPushButton("Activate")
        self._activate_btn.clicked.connect(self._activate_selected)
        btn_layout.addWidget(self._activate_btn)

        self._duplicate_btn = QPushButton("Duplicate")
        self._duplicate_btn.clicked.connect(self._duplicate_selected)
        btn_layout.addWidget(self._duplicate_btn)

        self._rename_btn = QPushButton("Rename")
        self._rename_btn.clicked.connect(self._rename_selected)
        btn_layout.addWidget(self._rename_btn)

        self._delete_btn = QPushButton("Delete")
        self._delete_btn.clicked.connect(self._delete_selected)
        btn_layout.addWidget(self._delete_btn)

        root.addWidget(btn_group)

        # --- Import / Export ---
        io_group = QGroupBox("Import / Export")
        io_layout = QHBoxLayout(io_group)

        self._import_btn = QPushButton("Import JSON…")
        self._import_btn.clicked.connect(self._import_json)
        io_layout.addWidget(self._import_btn)

        self._export_btn = QPushButton("Export JSON…")
        self._export_btn.clicked.connect(self._export_json)
        io_layout.addWidget(self._export_btn)

        root.addWidget(io_group)

        self._status_label = QLabel("")
        root.addWidget(self._status_label)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def add_profile(self, profile: Profile):
        """Add a profile to the list (does not save to disk)."""
        self._profiles.append(profile)
        item = QListWidgetItem(profile.name)
        item.setData(256, len(self._profiles) - 1)  # store index
        self._list.addItem(item)
        logger.debug("ProfileManager: added profile '%s'", profile.name)

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _selected_profile(self) -> Profile | None:
        items = self._list.selectedItems()
        if not items:
            return None
        idx = items[0].data(256)
        return self._profiles[idx] if 0 <= idx < len(self._profiles) else None

    def _selected_index(self) -> int:
        items = self._list.selectedItems()
        if not items:
            return -1
        return self._list.row(items[0])

    # ------------------------------------------------------------------
    # Slots
    # ------------------------------------------------------------------

    def _activate_selected(self):
        profile = self._selected_profile()
        if profile is None:
            QMessageBox.information(self, "No Selection", "Select a profile first.")
            return
        self._status_label.setText(f"Active: {profile.name}")
        logger.info("Profile activated: %s", profile.name)
        self.profile_activated.emit(profile)

    def _duplicate_selected(self):
        profile = self._selected_profile()
        if profile is None:
            return
        new_name, ok = QInputDialog.getText(
            self, "Duplicate Profile", "New profile name:",
            text=f"{profile.name} Copy"
        )
        if not ok or not new_name.strip():
            return
        new_name = new_name.strip()
        try:
            self._profile_manager.duplicate(profile.name, new_name)
            new_profile = self._profile_manager.load(new_name)
            self.add_profile(new_profile)
            self._status_label.setText(f"Duplicated as '{new_name}'.")
            logger.info("Profile duplicated: %s → %s", profile.name, new_name)
        except Exception as exc:
            logger.error("Duplicate failed: %s", exc)
            QMessageBox.critical(self, "Duplicate Error", str(exc))

    def _rename_selected(self):
        profile = self._selected_profile()
        if profile is None:
            return
        new_name, ok = QInputDialog.getText(
            self, "Rename Profile", "New name:", text=profile.name
        )
        if not ok or not new_name.strip():
            return
        new_name = new_name.strip()
        old_name = profile.name
        try:
            # Duplicate under new name, delete old
            self._profile_manager.duplicate(old_name, new_name)
            self._profile_manager.delete(old_name)
            profile.name = new_name
            # Update list item text
            row = self._selected_index()
            if row >= 0:
                self._list.item(row).setText(new_name)
            self._status_label.setText(f"Renamed to '{new_name}'.")
            logger.info("Profile renamed: %s → %s", old_name, new_name)
        except Exception as exc:
            logger.error("Rename failed: %s", exc)
            QMessageBox.critical(self, "Rename Error", str(exc))

    def _delete_selected(self):
        profile = self._selected_profile()
        if profile is None:
            return
        confirm = QMessageBox.question(
            self, "Delete Profile",
            f"Delete profile '{profile.name}'? This cannot be undone.",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if confirm != QMessageBox.StandardButton.Yes:
            return
        try:
            self._profile_manager.delete(profile.name)
            row = self._selected_index()
            if row >= 0:
                self._list.takeItem(row)
                self._profiles.pop(row)
                # Re-index remaining items
                for i in range(self._list.count()):
                    self._list.item(i).setData(256, i)
            self._status_label.setText(f"Deleted '{profile.name}'.")
            logger.info("Profile deleted: %s", profile.name)
        except Exception as exc:
            logger.error("Delete failed: %s", exc)
            QMessageBox.critical(self, "Delete Error", str(exc))

    def _import_json(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Import Profile JSON", "", "JSON files (*.json);;All files (*)"
        )
        if not path:
            return
        try:
            profile = Profile.import_json(Path(path))
            self._profile_manager.save(profile)
            self.add_profile(profile)
            self._status_label.setText(f"Imported '{profile.name}'.")
            logger.info("Profile imported from %s", path)
        except Exception as exc:
            logger.error("Import failed: %s", exc)
            QMessageBox.critical(self, "Import Error", str(exc))

    def _export_json(self):
        profile = self._selected_profile()
        if profile is None:
            QMessageBox.information(self, "No Selection", "Select a profile first.")
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export Profile JSON",
            f"{profile.name}.json",
            "JSON files (*.json);;All files (*)"
        )
        if not path:
            return
        try:
            profile.export_json(Path(path))
            self._status_label.setText(f"Exported to '{path}'.")
            logger.info("Profile exported to %s", path)
        except Exception as exc:
            logger.error("Export failed: %s", exc)
            QMessageBox.critical(self, "Export Error", str(exc))
