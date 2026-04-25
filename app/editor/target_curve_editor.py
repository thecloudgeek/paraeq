"""Target curve editor with preset selection and FIR/PEQ generation."""
import logging
from pathlib import Path

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import pyqtSignal
from PyQt6.QtWidgets import (
    QComboBox,
    QFileDialog,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QMessageBox,
    QPushButton,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)

logger = logging.getLogger(__name__)


class TargetCurveEditor(QWidget):
    """Panel for choosing target curves and generating correction filters."""

    correction_generated = pyqtSignal(dict)  # keys: type, data, freqs, magnitude_db

    def __init__(self, parent=None):
        super().__init__(parent)

        self._measured_freqs: np.ndarray | None = None
        self._measured_db: np.ndarray | None = None
        self._target_curve = None  # TargetCurve or None

        self._setup_ui()
        self._populate_presets()

    # ------------------------------------------------------------------
    # UI
    # ------------------------------------------------------------------

    def _setup_ui(self):
        root = QVBoxLayout(self)
        root.setSpacing(8)

        # --- Preset row ---
        preset_group = QGroupBox("Target Curve")
        preset_layout = QHBoxLayout(preset_group)

        preset_layout.addWidget(QLabel("Preset:"))
        self._preset_combo = QComboBox()
        self._preset_combo.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed
        )
        self._preset_combo.currentIndexChanged.connect(self._on_preset_changed)
        preset_layout.addWidget(self._preset_combo)

        self._import_csv_btn = QPushButton("Import CSV…")
        self._import_csv_btn.clicked.connect(self._import_csv)
        preset_layout.addWidget(self._import_csv_btn)

        self._export_csv_btn = QPushButton("Export CSV…")
        self._export_csv_btn.clicked.connect(self._export_csv)
        preset_layout.addWidget(self._export_csv_btn)

        root.addWidget(preset_group)

        # --- Generation row ---
        gen_group = QGroupBox("Generate Correction")
        gen_layout = QHBoxLayout(gen_group)

        self._gen_fir_btn = QPushButton("Generate FIR Filter")
        self._gen_fir_btn.clicked.connect(self._generate_fir)
        gen_layout.addWidget(self._gen_fir_btn)

        self._gen_peq_btn = QPushButton("Generate Parametric EQ")
        self._gen_peq_btn.clicked.connect(self._generate_peq)
        gen_layout.addWidget(self._gen_peq_btn)

        self._status_label = QLabel("Load a measurement first.")
        gen_layout.addWidget(self._status_label)

        root.addWidget(gen_group)

        # --- Plot ---
        self._plot_widget = pg.PlotWidget(title="Frequency Response vs Target")
        self._plot_widget.setLabel("left", "Magnitude (dB)")
        self._plot_widget.setLabel("bottom", "Frequency (Hz)")
        self._plot_widget.setLogMode(x=True, y=False)
        self._plot_widget.showGrid(x=True, y=True, alpha=0.3)
        self._plot_widget.setXRange(np.log10(20), np.log10(20_000))
        self._plot_widget.setYRange(-40, 20)
        self._plot_widget.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding
        )
        self._plot_widget.addLegend()

        self._meas_curve = self._plot_widget.plot(
            pen=pg.mkPen("#4FC3F7", width=2), name="Measured"
        )
        self._target_plot_curve = self._plot_widget.plot(
            pen=pg.mkPen("#FF9800", width=2), name="Target"
        )
        self._delta_curve = self._plot_widget.plot(
            pen=pg.mkPen("#66BB6A", width=1, style=pg.QtCore.Qt.PenStyle.DashLine),
            name="Correction delta",
        )

        root.addWidget(self._plot_widget)

    def _populate_presets(self):
        self._preset_combo.blockSignals(True)
        self._preset_combo.clear()
        self._builtin_targets = []

        self._preset_combo.addItem("Flat (0 dB)", userData=None)

        try:
            from paraeq.correction.target_curves import list_builtin_targets

            targets = list_builtin_targets()
            self._builtin_targets = targets
            for t in targets:
                self._preset_combo.addItem(t.name, userData=t)
        except Exception as exc:
            logger.warning("Could not load built-in targets: %s", exc)

        self._preset_combo.blockSignals(False)
        self._on_preset_changed(0)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    def set_measurement(self, freqs: np.ndarray, magnitude_db: np.ndarray):
        """Receive measurement data from the measurement wizard."""
        self._measured_freqs = freqs
        self._measured_db = magnitude_db
        self._update_plot()
        self._status_label.setText("Measurement loaded — select target and generate.")
        logger.info(
            "TargetCurveEditor: measurement set, freqs=%s", freqs.shape
        )

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _on_preset_changed(self, index: int):
        target = self._preset_combo.itemData(index)
        if target is None:
            # Flat: generate a trivial flat TargetCurve
            try:
                from paraeq.correction.target_curves import TargetCurve

                freqs = np.array([20.0, 20_000.0])
                gains = np.array([0.0, 0.0])
                self._target_curve = TargetCurve("Flat", freqs, gains)
            except Exception:
                self._target_curve = None
        else:
            self._target_curve = target

        self._update_plot()
        logger.debug("Target preset changed to index=%d", index)

    def _update_plot(self):
        if self._measured_freqs is not None and self._measured_db is not None:
            mask = self._measured_freqs > 0
            self._meas_curve.setData(
                self._measured_freqs[mask], self._measured_db[mask]
            )

            if self._target_curve is not None:
                target_db = self._target_curve.interpolate(self._measured_freqs)
                self._target_plot_curve.setData(
                    self._measured_freqs[mask], target_db[mask]
                )
                delta = target_db - self._measured_db
                self._delta_curve.setData(
                    self._measured_freqs[mask], delta[mask]
                )
            else:
                self._target_plot_curve.setData([], [])
                self._delta_curve.setData([], [])
        else:
            self._meas_curve.setData([], [])
            self._target_plot_curve.setData([], [])
            self._delta_curve.setData([], [])

    def _compute_correction(self) -> tuple[np.ndarray, np.ndarray] | None:
        """Return (correction_db, freqs) or None if not ready."""
        if self._measured_freqs is None or self._measured_db is None:
            QMessageBox.warning(self, "No Measurement", "Load a measurement first.")
            return None
        if self._target_curve is None:
            QMessageBox.warning(self, "No Target", "Select a target curve first.")
            return None

        from paraeq.correction.target_curves import compute_correction

        target_db = self._target_curve.interpolate(self._measured_freqs)
        correction_db = compute_correction(self._measured_db, target_db)
        return correction_db, self._measured_freqs

    def _generate_fir(self):
        result = self._compute_correction()
        if result is None:
            return
        correction_db, freqs = result

        try:
            from paraeq.correction.fir_filter import design_fir_correction

            fir = design_fir_correction(correction_db, freqs)
            logger.info("FIR correction designed, length=%d", len(fir))
            self._status_label.setText(f"FIR filter generated ({len(fir)} taps).")
            self.correction_generated.emit(
                {
                    "type": "fir",
                    "data": fir,
                    "freqs": freqs,
                    "magnitude_db": correction_db,
                }
            )
        except Exception as exc:
            logger.error("FIR generation failed: %s", exc)
            QMessageBox.critical(self, "FIR Error", str(exc))

    def _generate_peq(self):
        result = self._compute_correction()
        if result is None:
            return
        correction_db, freqs = result

        try:
            from paraeq.correction.auto_fit import auto_fit_parametric_eq

            bands = auto_fit_parametric_eq(correction_db, freqs, sample_rate=48_000)
            logger.info("PEQ auto-fit complete: %d bands", len(bands))
            self._status_label.setText(
                f"Parametric EQ generated ({len(bands)} bands)."
            )
            self.correction_generated.emit(
                {
                    "type": "peq",
                    "data": bands,
                    "freqs": freqs,
                    "magnitude_db": correction_db,
                }
            )
        except Exception as exc:
            logger.error("PEQ generation failed: %s", exc)
            QMessageBox.critical(self, "PEQ Error", str(exc))

    def _import_csv(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Import Target CSV", "", "CSV files (*.csv);;All files (*)"
        )
        if not path:
            return
        try:
            from paraeq.correction.target_curves import load_target_csv

            target = load_target_csv(Path(path))
            # Add to combo and select it
            self._preset_combo.blockSignals(True)
            self._preset_combo.addItem(f"[imported] {target.name}", userData=target)
            self._preset_combo.setCurrentIndex(self._preset_combo.count() - 1)
            self._preset_combo.blockSignals(False)
            self._target_curve = target
            self._update_plot()
            logger.info("Imported target CSV: %s", path)
        except Exception as exc:
            logger.error("Failed to import CSV: %s", exc)
            QMessageBox.critical(self, "Import Error", str(exc))

    def _export_csv(self):
        if self._target_curve is None:
            QMessageBox.warning(self, "No Target", "No target curve to export.")
            return
        path, _ = QFileDialog.getSaveFileName(
            self, "Export Target CSV", f"{self._target_curve.name}.csv",
            "CSV files (*.csv);;All files (*)"
        )
        if not path:
            return
        try:
            with open(path, "w") as f:
                f.write("# frequency_hz,gain_db\n")
                for freq, gain in zip(
                    self._target_curve.frequencies, self._target_curve.gains_db
                ):
                    f.write(f"{freq:.4f},{gain:.4f}\n")
            logger.info("Exported target CSV to: %s", path)
        except Exception as exc:
            logger.error("Failed to export CSV: %s", exc)
            QMessageBox.critical(self, "Export Error", str(exc))
