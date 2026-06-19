"""Manual parametric EQ editor with band table and AutoEQ import/export."""
import logging
from pathlib import Path

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import Qt, pyqtSignal
from PyQt6.QtWidgets import (
    QComboBox,
    QDoubleSpinBox,
    QFileDialog,
    QGroupBox,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QMessageBox,
    QPushButton,
    QSizePolicy,
    QSpinBox,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

logger = logging.getLogger(__name__)

_INTERNAL_TYPES = ["peaking", "low_shelf", "high_shelf", "notch"]

# Plot colours per band (cycles)
_BAND_COLORS = [
    "#EF5350", "#FFA726", "#FFEE58", "#66BB6A",
    "#26C6DA", "#42A5F5", "#AB47BC", "#EC407A",
]

SAMPLE_RATE = 48_000
FREQS_PLOT = np.logspace(np.log10(20), np.log10(20_000), 512)

COL_TYPE = 0
COL_FREQ = 1
COL_GAIN = 2
COL_Q = 3


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# Main widget
# ---------------------------------------------------------------------------


class ManualEQEditor(QWidget):
    """Parametric EQ editor: table of bands + composite/individual FR plot."""

    eq_changed = pyqtSignal(list)  # list of EQBand
    preamp_changed = pyqtSignal(float)  # AutoEq preamp in dB

    def __init__(self, parent=None):
        super().__init__(parent)
        self._band_plot_items: list = []
        self._setup_ui()

    # ------------------------------------------------------------------
    # UI
    # ------------------------------------------------------------------

    def _setup_ui(self):
        root = QVBoxLayout(self)
        root.setSpacing(6)

        # --- Toolbar ---
        tb = QHBoxLayout()
        self._add_btn = QPushButton("Add Band")
        self._add_btn.clicked.connect(self._add_band)
        tb.addWidget(self._add_btn)

        self._remove_btn = QPushButton("Remove Band")
        self._remove_btn.clicked.connect(self._remove_band)
        tb.addWidget(self._remove_btn)

        tb.addStretch()

        self._browse_btn = QPushButton("Browse AutoEQ DB…")
        self._browse_btn.clicked.connect(self._browse_autoeq_db)
        tb.addWidget(self._browse_btn)

        self._import_btn = QPushButton("Import AutoEQ…")
        self._import_btn.clicked.connect(self._import_autoeq)
        tb.addWidget(self._import_btn)

        self._export_btn = QPushButton("Export AutoEQ…")
        self._export_btn.clicked.connect(self._export_autoeq)
        tb.addWidget(self._export_btn)

        root.addLayout(tb)

        # --- Band table ---
        self._table = QTableWidget(0, 4)
        self._table.setHorizontalHeaderLabels(["Type", "Freq (Hz)", "Gain (dB)", "Q"])
        self._table.horizontalHeader().setSectionResizeMode(
            QHeaderView.ResizeMode.Stretch
        )
        self._table.setSelectionBehavior(
            self._table.SelectionBehavior.SelectRows
        )
        self._table.setMaximumHeight(200)
        self._table.itemChanged.connect(self._on_table_changed)
        root.addWidget(self._table)

        # --- Plot ---
        from app.plot_utils import AudioFreqAxis

        self._plot_widget = pg.PlotWidget(
            title="Parametric EQ Response",
            axisItems={"bottom": AudioFreqAxis(orientation="bottom")},
        )
        self._plot_widget.setLabel("left", "Magnitude (dB)")
        self._plot_widget.setLabel("bottom", "Frequency (Hz)")
        self._plot_widget.setLogMode(x=True, y=False)
        self._plot_widget.showGrid(x=True, y=True, alpha=0.3)
        self._plot_widget.setXRange(np.log10(20), np.log10(20_000))
        self._plot_widget.setYRange(-20, 20)
        self._plot_widget.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding
        )
        self._plot_widget.addLegend(offset=(10, 10))
        self._composite_curve = self._plot_widget.plot(
            pen=pg.mkPen("w", width=2.5), name="Composite (applied)"
        )
        root.addWidget(self._plot_widget)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def load_bands(self, bands):
        """Load a list of EQBand objects into the editor."""
        self._table.blockSignals(True)
        self._table.setRowCount(0)
        for band in bands:
            self._append_row(
                band.filter_type, band.fc, band.gain_db, band.q
            )
        self._table.blockSignals(False)
        self._refresh_plot()
        logger.info("ManualEQEditor: loaded %d bands", len(bands))

    # ------------------------------------------------------------------
    # Slots
    # ------------------------------------------------------------------

    def _add_band(self):
        self._table.blockSignals(True)
        self._append_row("peaking", 1_000.0, 0.0, 1.41)
        self._table.blockSignals(False)
        self._refresh_plot()

    def _remove_band(self):
        rows = sorted(
            {idx.row() for idx in self._table.selectedIndexes()}, reverse=True
        )
        if not rows:
            if self._table.rowCount() > 0:
                rows = [self._table.rowCount() - 1]
        self._table.blockSignals(True)
        for row in rows:
            self._table.removeRow(row)
        self._table.blockSignals(False)
        self._refresh_plot()

    def _on_table_changed(self, item):
        self._refresh_plot()

    # ------------------------------------------------------------------
    # Table helpers
    # ------------------------------------------------------------------

    def _append_row(self, filter_type: str, fc: float, gain_db: float, q: float):
        row = self._table.rowCount()
        self._table.insertRow(row)

        # Type cell — QComboBox
        combo = QComboBox()
        for t in _INTERNAL_TYPES:
            combo.addItem(t)
        combo.setCurrentText(filter_type)
        combo.currentIndexChanged.connect(self._refresh_plot)
        self._table.setCellWidget(row, COL_TYPE, combo)

        # Freq
        freq_item = QTableWidgetItem(f"{fc:.1f}")
        freq_item.setTextAlignment(Qt.AlignmentFlag.AlignCenter)
        self._table.setItem(row, COL_FREQ, freq_item)

        # Gain
        gain_item = QTableWidgetItem(f"{gain_db:.1f}")
        gain_item.setTextAlignment(Qt.AlignmentFlag.AlignCenter)
        self._table.setItem(row, COL_GAIN, gain_item)

        # Q
        q_item = QTableWidgetItem(f"{q:.3f}")
        q_item.setTextAlignment(Qt.AlignmentFlag.AlignCenter)
        self._table.setItem(row, COL_Q, q_item)

    def _read_bands(self) -> list:
        """Read all rows and return list of EQBand (skip malformed rows)."""
        from paraeq.correction.parametric_eq import EQBand

        bands = []
        for row in range(self._table.rowCount()):
            try:
                combo = self._table.cellWidget(row, COL_TYPE)
                filter_type = combo.currentText() if combo else "peaking"
                fc = float(self._table.item(row, COL_FREQ).text())
                gain_db = float(self._table.item(row, COL_GAIN).text())
                q = float(self._table.item(row, COL_Q).text())
                bands.append(EQBand(filter_type=filter_type, fc=fc, gain_db=gain_db, q=q))
            except Exception as exc:
                logger.debug("Skipping malformed row %d: %s", row, exc)
        return bands

    # ------------------------------------------------------------------
    # Plot
    # ------------------------------------------------------------------

    def _refresh_plot(self):
        # Remove old per-band items
        for item in self._band_plot_items:
            self._plot_widget.removeItem(item)
        self._band_plot_items = []

        bands = self._read_bands()

        composite_db = np.zeros(len(FREQS_PLOT))
        for i, band in enumerate(bands):
            try:
                from paraeq.correction.parametric_eq import ParametricEQ

                peq = ParametricEQ([band], SAMPLE_RATE)
                band_db = peq.frequency_response(FREQS_PLOT)
                composite_db += band_db

                color = _BAND_COLORS[i % len(_BAND_COLORS)]
                curve = self._plot_widget.plot(
                    FREQS_PLOT, band_db,
                    pen=pg.mkPen(color, width=1, style=pg.QtCore.Qt.PenStyle.DotLine),
                    name=f"Band {i+1}",
                )
                self._band_plot_items.append(curve)
            except Exception as exc:
                logger.debug("Band %d plot error: %s", i, exc)

        self._composite_curve.setData(FREQS_PLOT, composite_db)
        self.eq_changed.emit(bands)
        logger.debug("ManualEQEditor: refreshed plot with %d bands", len(bands))

    # ------------------------------------------------------------------
    # Import / Export
    # ------------------------------------------------------------------

    def _browse_autoeq_db(self):
        from app.eq.autoeq_browser import AutoEQBrowserDialog

        dlg = AutoEQBrowserDialog(self)
        try:
            if dlg.exec() != dlg.DialogCode.Accepted:
                return
            bands = dlg.selected_bands
            preamp_db = dlg.selected_preamp_db
        finally:
            dlg.deleteLater()  # parented to self, so won't free on scope exit otherwise

        if not bands:
            QMessageBox.warning(self, "AutoEQ", "That preset had no usable bands.")
            return
        self.load_bands(bands)
        self.preamp_changed.emit(preamp_db)
        logger.info(
            "Loaded AutoEq preset: %d bands, preamp %.1f dB", len(bands), preamp_db
        )

    def _import_autoeq(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Import AutoEQ Preset", "",
            "Text files (*.txt *.csv);;All files (*)"
        )
        if not path:
            return
        try:
            from paraeq.correction.autoeq_db import parse_parametric_eq

            parsed = parse_parametric_eq(Path(path).read_text())
            if not parsed.bands:
                QMessageBox.warning(
                    self, "Import", "No AutoEQ filter lines found in the file."
                )
                return
            self._table.blockSignals(True)
            self._table.setRowCount(0)
            for band in parsed.bands:
                self._append_row(band.filter_type, band.fc, band.gain_db, band.q)
            self._table.blockSignals(False)
            self._refresh_plot()
            logger.info(
                "Imported %d bands (preamp %.1f dB) from %s",
                len(parsed.bands), parsed.preamp_db, path,
            )
        except Exception as exc:
            logger.error("AutoEQ import failed: %s", exc)
            QMessageBox.critical(self, "Import Error", str(exc))

    def _export_autoeq(self):
        path, _ = QFileDialog.getSaveFileName(
            self, "Export AutoEQ Preset", "eq_preset.txt",
            "Text files (*.txt);;All files (*)"
        )
        if not path:
            return
        try:
            from paraeq.correction.parametric_eq import ParametricEQ

            bands = self._read_bands()
            peq = ParametricEQ(bands, SAMPLE_RATE)
            text = peq.export_autoeq_format()
            Path(path).write_text(text)
            logger.info("Exported AutoEQ preset to %s", path)
        except Exception as exc:
            logger.error("AutoEQ export failed: %s", exc)
            QMessageBox.critical(self, "Export Error", str(exc))
