"""Searchable AutoEq headphone-preset browser dialog.

Network I/O runs on a QThread worker (the AutoEQClient is synchronous). On
accept, the selected model's parsed bands + preamp are exposed via
``selected_bands`` / ``selected_preamp_db`` for the caller to apply.
"""

import logging

from PyQt6.QtCore import Qt, QThread, pyqtSignal
from PyQt6.QtGui import QCloseEvent
from PyQt6.QtWidgets import (
    QDialog,
    QDialogButtonBox,
    QLabel,
    QLineEdit,
    QListWidget,
    QListWidgetItem,
    QVBoxLayout,
)

logger = logging.getLogger(__name__)

_ATTRIBUTION = (
    "Presets from the AutoEq project (github.com/jaakkopasanen/AutoEq), "
    "measured by oratory1990, crinacle, Rtings, and others."
)


class _AutoEQWorker(QThread):
    """Runs one AutoEQClient call off the GUI thread."""

    index_ready = pyqtSignal(list)        # list[AutoEQEntry]
    preset_ready = pyqtSignal(object)     # ParsedPreset
    failed = pyqtSignal(str)

    def __init__(self, client, mode, entry=None, parent=None):
        super().__init__(parent)
        self._client = client
        self._mode = mode                 # "index" or "preset"
        self._entry = entry

    def run(self):
        try:
            if self._mode == "index":
                self.index_ready.emit(self._client.fetch_index())
            else:
                self.preset_ready.emit(self._client.fetch_preset(self._entry))
        except Exception as exc:  # noqa: BLE001 — surfaced to the UI
            logger.error("AutoEq worker (%s) failed: %s", self._mode, exc)
            self.failed.emit(str(exc))


class AutoEQBrowserDialog(QDialog):
    def __init__(self, parent=None, client=None):
        super().__init__(parent)
        self.setWindowTitle("Browse AutoEQ Database")
        self.resize(520, 560)

        if client is None:
            from paraeq.correction.autoeq_db import AutoEQClient

            client = AutoEQClient()
        self._client = client
        self._entries = []
        self._worker = None

        self.selected_bands = []
        self.selected_preamp_db = 0.0

        self._build_ui()
        self._load_index()

    def _build_ui(self):
        layout = QVBoxLayout(self)
        self._search = QLineEdit()
        self._search.setPlaceholderText("Search headphone model…")
        self._search.textChanged.connect(self._apply_filter)
        layout.addWidget(self._search)

        self._list = QListWidget()
        self._list.itemDoubleClicked.connect(lambda _i: self._accept_selection())
        layout.addWidget(self._list, 1)

        self._status = QLabel("Loading index…")
        layout.addWidget(self._status)

        attribution = QLabel(_ATTRIBUTION)
        attribution.setWordWrap(True)
        attribution.setStyleSheet("color: gray; font-size: 10px;")
        layout.addWidget(attribution)

        self._buttons = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        self._buttons.accepted.connect(self._accept_selection)
        self._buttons.rejected.connect(self.reject)
        layout.addWidget(self._buttons)

    # --- worker lifecycle ---
    def _start_worker(self, mode, entry=None) -> "_AutoEQWorker":
        """Create a parented worker (so Qt owns it) that self-deletes on finish."""
        worker = _AutoEQWorker(self._client, mode, entry, parent=self)
        worker.finished.connect(worker.deleteLater)
        self._worker = worker
        return worker

    def _shutdown_worker(self):
        """Block until any in-flight worker finishes, so it is never destroyed
        while running (which Qt turns into a hard process abort)."""
        worker = self._worker
        if worker is not None and worker.isRunning():
            worker.wait()

    def reject(self):
        self._shutdown_worker()
        super().reject()

    def closeEvent(self, event: QCloseEvent):
        self._shutdown_worker()
        super().closeEvent(event)

    # --- index ---
    def _load_index(self):
        worker = self._start_worker("index")
        worker.index_ready.connect(self._on_index_ready)
        worker.failed.connect(self._on_failed)
        worker.start()

    def _on_index_ready(self, entries):
        self._entries = entries
        self._status.setText(f"{len(entries)} models. Double-click or select + OK.")
        self._apply_filter(self._search.text())

    def _apply_filter(self, text):
        needle = text.strip().lower()
        self._list.clear()
        for entry in self._entries:
            if needle and needle not in entry.name.lower():
                continue
            item = QListWidgetItem(f"{entry.name}  —  {entry.source} / {entry.rig}")
            item.setData(Qt.ItemDataRole.UserRole, entry)
            self._list.addItem(item)

    # --- preset ---
    def _accept_selection(self):
        if self._worker is not None and self._worker.isRunning():
            return  # a fetch is already in flight
        item = self._list.currentItem()
        if item is None:
            self._status.setText("Select a model first.")
            return
        entry = item.data(Qt.ItemDataRole.UserRole)
        self._status.setText(f"Fetching {entry.name}…")
        self._buttons.setEnabled(False)
        worker = self._start_worker("preset", entry)
        worker.preset_ready.connect(self._on_preset_ready)
        worker.failed.connect(self._on_failed)
        worker.start()

    def _on_preset_ready(self, parsed):
        self.selected_bands = parsed.bands
        self.selected_preamp_db = parsed.preamp_db
        logger.info(
            "AutoEq preset selected: %d bands, preamp %.1f dB",
            len(parsed.bands), parsed.preamp_db,
        )
        self.accept()

    def _on_failed(self, message):
        self._buttons.setEnabled(True)
        self._status.setText(f"Error: {message}")
