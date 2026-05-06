"""Measurement wizard: device selection, sweep, recording, FR display, averaging."""
import logging

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import QThread, Qt, pyqtSignal
from PyQt6.QtWidgets import (
    QComboBox,
    QFileDialog,
    QGroupBox,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QMessageBox,
    QProgressBar,
    QPushButton,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Background worker
# ---------------------------------------------------------------------------


class MeasurementWorker(QThread):
    """Plays a log-sine sweep and records simultaneously in a background thread."""

    progress = pyqtSignal(int)           # 0–100
    finished = pyqtSignal(np.ndarray, int)  # impulse_response, sample_rate
    error = pyqtSignal(str)

    SWEEP_DURATION = 5.0
    SAMPLE_RATE = 48_000

    def __init__(
        self,
        output_device: int | None,
        input_device: int | None,
        parent=None,
    ):
        super().__init__(parent)
        self._output_device = output_device
        self._input_device = input_device

    def run(self):
        try:
            import sounddevice as sd

            from paraeq.measurement.deconvolution import deconvolve
            from paraeq.measurement.sweep import generate_sweep

            sr = self.SAMPLE_RATE
            self.progress.emit(5)
            logger.info(
                "MeasurementWorker: generating sweep (duration=%.1fs, sr=%d)",
                self.SWEEP_DURATION,
                sr,
            )
            sweep = generate_sweep(self.SWEEP_DURATION, sr)
            self.progress.emit(10)

            sweep_stereo = np.column_stack([sweep, sweep]).astype(np.float32)
            logger.info(
                "MeasurementWorker: playrec (out=%s, in=%s)",
                self._output_device,
                self._input_device,
            )
            recording = sd.playrec(
                sweep_stereo,
                samplerate=sr,
                channels=1,
                input_mapping=[1],
                output_mapping=[1, 2],
                device=(self._output_device, self._input_device),
                dtype="float32",
            )
            self.progress.emit(50)
            sd.wait()
            self.progress.emit(80)

            recorded_mono = recording[:, 0].astype(np.float64)
            logger.info(
                "MeasurementWorker: deconvolving (recorded %d samples)",
                len(recorded_mono),
            )
            ir = deconvolve(recorded_mono, sweep, sr)
            self.progress.emit(95)
            logger.info("MeasurementWorker: done, IR length=%d", len(ir))
            self.finished.emit(ir, sr)
            self.progress.emit(100)
        except Exception as exc:  # pragma: no cover
            logger.error("MeasurementWorker error: %s", exc, exc_info=True)
            self.error.emit(str(exc))


# ---------------------------------------------------------------------------
# Main widget
# ---------------------------------------------------------------------------


class MeasurementWizard(QWidget):
    """Full measurement wizard panel."""

    measurement_complete = pyqtSignal(dict)

    def __init__(self, parent=None):
        super().__init__(parent)
        self._measurements_db: list[np.ndarray] = []
        self._current_freqs: np.ndarray | None = None
        self._current_ir: np.ndarray | None = None
        self._current_sr: int = 48_000
        self._compensation_path: str | None = None
        self._worker: MeasurementWorker | None = None

        self._setup_ui()
        self._populate_devices()

    # ------------------------------------------------------------------
    # UI
    # ------------------------------------------------------------------

    def _setup_ui(self):
        root = QVBoxLayout(self)
        root.setSpacing(8)

        # --- Device selection ---
        dev_group = QGroupBox("Audio Devices")
        dev_layout = QHBoxLayout(dev_group)

        dev_layout.addWidget(QLabel("Output:"))
        self._output_combo = QComboBox()
        self._output_combo.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed
        )
        dev_layout.addWidget(self._output_combo)

        dev_layout.addWidget(QLabel("Input:"))
        self._input_combo = QComboBox()
        self._input_combo.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed
        )
        dev_layout.addWidget(self._input_combo)

        root.addWidget(dev_group)

        # --- Headphone info ---
        info_group = QGroupBox("Headphone")
        info_layout = QHBoxLayout(info_group)

        info_layout.addWidget(QLabel("Model name:"))
        self._name_edit = QLineEdit()
        self._name_edit.setPlaceholderText("e.g. HD 650")
        info_layout.addWidget(self._name_edit)

        self._comp_btn = QPushButton("Load Compensation…")
        self._comp_btn.clicked.connect(self._load_compensation)
        self._comp_label = QLabel("No compensation file loaded")
        info_layout.addWidget(self._comp_btn)
        info_layout.addWidget(self._comp_label)

        root.addWidget(info_group)

        # --- Controls ---
        ctrl_layout = QHBoxLayout()
        self._start_btn = QPushButton("Start Measurement")
        self._start_btn.clicked.connect(self._start_measurement)
        ctrl_layout.addWidget(self._start_btn)

        self._again_btn = QPushButton("Measure Again (average)")
        self._again_btn.setEnabled(False)
        self._again_btn.clicked.connect(self._start_measurement)
        ctrl_layout.addWidget(self._again_btn)

        self._save_btn = QPushButton("Save to Profile")
        self._save_btn.setEnabled(False)
        self._save_btn.clicked.connect(self._save_to_profile)
        ctrl_layout.addWidget(self._save_btn)

        root.addLayout(ctrl_layout)

        # --- Progress ---
        self._progress = QProgressBar()
        self._progress.setRange(0, 100)
        self._progress.setValue(0)
        self._progress.setVisible(False)
        root.addWidget(self._progress)

        self._status_label = QLabel("Select devices and press Start Measurement.")
        root.addWidget(self._status_label)

        # --- FR plot ---
        self._plot_widget = pg.PlotWidget(title="Frequency Response")
        self._plot_widget.setLabel("left", "Magnitude (dB)")
        self._plot_widget.setLabel("bottom", "Frequency (Hz)")
        self._plot_widget.setLogMode(x=True, y=False)
        self._plot_widget.showGrid(x=True, y=True, alpha=0.3)
        self._plot_widget.setXRange(np.log10(20), np.log10(20_000))
        self._plot_widget.setYRange(-40, 20)
        self._plot_widget.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding
        )

        self._fr_curve = self._plot_widget.plot(
            pen=pg.mkPen("#4FC3F7", width=2), name="FR"
        )
        root.addWidget(self._plot_widget)

        self._avg_label = QLabel("Measurements averaged: 0")
        root.addWidget(self._avg_label)

    def _populate_devices(self):
        try:
            from paraeq.audio.devices import list_input_devices, list_output_devices

            self._output_devices = list_output_devices()
            self._input_devices = list_input_devices()
        except Exception as exc:
            logger.warning("Could not enumerate audio devices: %s", exc)
            self._output_devices = []
            self._input_devices = []

        self._output_combo.clear()
        for dev in self._output_devices:
            self._output_combo.addItem(dev.name, userData=dev.index)

        self._input_combo.clear()
        for dev in self._input_devices:
            self._input_combo.addItem(dev.name, userData=dev.index)

    # ------------------------------------------------------------------
    # Slots
    # ------------------------------------------------------------------

    def _load_compensation(self):
        path, _ = QFileDialog.getOpenFileName(
            self, "Load Compensation File", "", "CSV files (*.csv);;All files (*)"
        )
        if path:
            self._compensation_path = path
            self._comp_label.setText(path.split("/")[-1])
            logger.info("Compensation file set: %s", path)

    def _start_measurement(self):
        if self._worker is not None and self._worker.isRunning():
            self._status_label.setText("Measurement already in progress…")
            return

        output_idx = self._output_combo.currentData()
        input_idx = self._input_combo.currentData()

        self._start_btn.setEnabled(False)
        self._again_btn.setEnabled(False)
        self._save_btn.setEnabled(False)
        self._progress.setVisible(True)
        self._progress.setValue(0)
        self._status_label.setText("Running sweep…")

        self._worker = MeasurementWorker(output_idx, input_idx, parent=self)
        self._worker.progress.connect(self._progress.setValue)
        self._worker.finished.connect(self._on_worker_finished)
        self._worker.error.connect(self._on_worker_error)
        self._worker.start()

    def _on_worker_finished(self, ir: np.ndarray, sample_rate: int):
        from paraeq.measurement.frequency_response import (
            average_measurements,
            compute_frequency_response,
            fractional_octave_smooth,
        )

        self._current_ir = ir
        self._current_sr = sample_rate

        # Apply compensation if loaded
        if self._compensation_path:
            try:
                from paraeq.measurement.compensation import (
                    apply_compensation,
                    load_compensation,
                )

                comp = load_compensation(self._compensation_path)
                ir = apply_compensation(ir, comp, sample_rate)
                logger.info("Compensation applied from %s", self._compensation_path)
            except Exception as exc:
                logger.warning("Could not apply compensation: %s", exc)

        freqs, mag_db = compute_frequency_response(ir, sample_rate)
        mag_db_smooth = fractional_octave_smooth(mag_db, freqs, fraction=6)

        self._measurements_db.append(mag_db_smooth)
        avg_db = average_measurements(self._measurements_db)
        self._current_freqs = freqs

        # Update plot (skip DC bin)
        mask = freqs > 0
        self._fr_curve.setData(freqs[mask], avg_db[mask])

        n = len(self._measurements_db)
        self._avg_label.setText(f"Measurements averaged: {n}")
        self._status_label.setText(
            f"Measurement complete ({n} averaged). You can measure again or save."
        )

        self._start_btn.setEnabled(True)
        self._again_btn.setEnabled(True)
        self._save_btn.setEnabled(True)
        self._progress.setVisible(False)

        logger.info(
            "Measurement finished: %d averaged, freqs shape=%s",
            n,
            freqs.shape,
        )

        # Emit aggregated result
        self.measurement_complete.emit(
            {
                "frequencies": freqs,
                "magnitude_db": avg_db,
                "impulse_responses": [self._current_ir],
                "sample_rate": sample_rate,
                "headphone_model": self._name_edit.text() or "Unknown",
            }
        )

    def _on_worker_error(self, msg: str):
        logger.error("Measurement error: %s", msg)
        self._status_label.setText(f"Error: {msg}")
        self._start_btn.setEnabled(True)
        self._again_btn.setEnabled(bool(self._measurements_db))
        self._save_btn.setEnabled(bool(self._measurements_db))
        self._progress.setVisible(False)
        QMessageBox.critical(self, "Measurement Error", msg)

    def _save_to_profile(self):
        if self._current_ir is None:
            return

        from paraeq.profiles.profile import Profile, ProfileManager

        model = self._name_edit.text() or "Unknown"
        profile = Profile(name=model, headphone_model=model)
        profile.set_measurement(self._current_ir, self._current_sr)

        try:
            pm = ProfileManager()
            pm.save(profile)
            self._status_label.setText(f"Profile '{model}' saved.")
            logger.info("Profile saved: %s", model)
        except Exception as exc:
            logger.error("Failed to save profile: %s", exc)
            QMessageBox.critical(self, "Save Error", str(exc))
