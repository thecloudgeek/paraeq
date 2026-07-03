"""Measurement wizard: device selection, sweep, recording, FR display, averaging."""
import logging

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import QThread, QTimer, Qt, pyqtSignal
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
    SWEEP_AMPLITUDE = 0.5  # -6 dBFS — hearing-safe headroom over full-scale

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
            sweep = generate_sweep(self.SWEEP_DURATION, sr) * self.SWEEP_AMPLITUDE
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
                channels=2,
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
    save_profile_requested = pyqtSignal(dict)  # keys: name, ir, sample_rate

    def __init__(self, parent=None):
        super().__init__(parent)
        self._measurements_db: list[np.ndarray] = []
        self._current_freqs: np.ndarray | None = None
        self._current_ir: np.ndarray | None = None
        self._current_sr: int = 48_000
        self._compensation_path: str | None = None
        self._worker: MeasurementWorker | None = None

        self._test_stream = None  # sounddevice.Stream while test tone is playing
        self._test_pink_buffer: np.ndarray | None = None
        self._test_buf_pos = 0
        self._test_latest_db = -120.0
        self._test_meter_timer = QTimer(self)
        self._test_meter_timer.setInterval(33)  # ~30 Hz UI update
        self._test_meter_timer.timeout.connect(self._update_level_meter)

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
        self._test_level_btn = QPushButton("Test Level (pink noise)")
        self._test_level_btn.setToolTip(
            "Plays looping pink noise at the same source level as the sweep. "
            "Watch the input meter and aim for -28 to -18 dBFS RMS — pink noise "
            "has a high crest factor, so this maps to a sweep RMS around -19 to "
            "-9 dBFS (the ideal measurement window). Click again to stop."
        )
        self._test_level_btn.clicked.connect(self._toggle_test_tone)
        ctrl_layout.addWidget(self._test_level_btn)

        self._start_btn = QPushButton("Start Measurement")
        self._start_btn.clicked.connect(self._start_measurement)
        ctrl_layout.addWidget(self._start_btn)

        self._again_btn = QPushButton("Measure Again (average)")
        self._again_btn.setEnabled(False)
        self._again_btn.clicked.connect(self._measure_again)
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

        # --- Input level meter ---
        meter_layout = QHBoxLayout()
        meter_layout.addWidget(QLabel("Input level:"))
        self._level_meter = QProgressBar()
        self._level_meter.setRange(0, 600)  # maps -60..0 dBFS as (db + 60) * 10
        self._level_meter.setValue(0)
        self._level_meter.setFormat("-- dBFS")
        self._level_meter.setTextVisible(True)
        self._level_meter.setStyleSheet(
            """
            QProgressBar { border: 1px solid #444; border-radius: 3px; text-align: center; }
            QProgressBar::chunk {
                background: qlineargradient(x1:0, y1:0, x2:1, y2:0,
                    stop:0 #2E7D32, stop:0.7 #2E7D32,
                    stop:0.8 #F9A825, stop:0.95 #F9A825,
                    stop:0.96 #C62828, stop:1 #C62828);
            }
            """
        )
        meter_layout.addWidget(self._level_meter, 1)
        root.addLayout(meter_layout)

        # --- FR plot ---
        from app.plot_utils import AudioFreqAxis

        self._plot_widget = pg.PlotWidget(
            title="Frequency Response",
            axisItems={"bottom": AudioFreqAxis(orientation="bottom")},
        )
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

    def _toggle_test_tone(self):
        if self._test_stream is not None:
            self._stop_test_tone()
        else:
            self._start_test_tone()

    def _start_test_tone(self):
        import sounddevice as sd

        from paraeq.measurement.noise import generate_pink_noise

        sr = MeasurementWorker.SAMPLE_RATE
        amp = MeasurementWorker.SWEEP_AMPLITUDE
        # Pre-generate 2s of pink noise; the stream callback loops it.
        pink = (generate_pink_noise(2.0, sr) * amp).astype(np.float32)
        self._test_pink_buffer = pink
        self._test_buf_pos = 0
        self._test_latest_db = -120.0

        output_idx = self._output_combo.currentData()
        input_idx = self._input_combo.currentData()

        def callback(indata, outdata, frames, time_info, status):
            if status:
                logger.debug("Test stream status: %s", status)
            buf = self._test_pink_buffer
            n = len(buf)
            pos = self._test_buf_pos
            end = pos + frames
            if end <= n:
                chunk = buf[pos:end]
            else:
                chunk = np.concatenate([buf[pos:], buf[: end - n]])
            outdata[:, 0] = chunk
            outdata[:, 1] = chunk
            self._test_buf_pos = end % n

            # Use input channel 1 (left ear capsule on EARS) for level
            x = indata[:, 0]
            rms = float(np.sqrt(np.mean(x * x)))
            self._test_latest_db = 20.0 * np.log10(max(rms, 1e-10))

        try:
            self._test_stream = sd.Stream(
                samplerate=sr,
                channels=2,
                device=(output_idx, input_idx),
                callback=callback,
                dtype="float32",
            )
            self._test_stream.start()
        except Exception as exc:
            logger.error("Test tone stream failed: %s", exc)
            self._status_label.setText(f"Test tone failed: {exc}")
            self._test_stream = None
            return

        self._test_level_btn.setText("Stop Test")
        self._start_btn.setEnabled(False)
        self._again_btn.setEnabled(False)
        self._status_label.setText(
            "Pink noise playing — aim for -28 to -18 dBFS RMS on the meter."
        )
        self._test_meter_timer.start()
        logger.info(
            "Test tone started: amp=%.2f, out=%s, in=%s", amp, output_idx, input_idx
        )

    def _stop_test_tone(self):
        self._test_meter_timer.stop()
        if self._test_stream is not None:
            try:
                self._test_stream.stop()
                self._test_stream.close()
            except Exception as exc:
                logger.warning("Error closing test stream: %s", exc)
            self._test_stream = None
        self._level_meter.setValue(0)
        self._level_meter.setFormat("-- dBFS")
        self._test_level_btn.setText("Test Level (pink noise)")
        self._start_btn.setEnabled(True)
        self._again_btn.setEnabled(len(self._measurements_db) > 0)
        self._status_label.setText("Test tone stopped.")
        logger.info("Test tone stopped")

    def _update_level_meter(self):
        db = self._test_latest_db
        # Map -60..0 dBFS to 0..600 for QProgressBar
        clamped = max(-60.0, min(0.0, db))
        self._level_meter.setValue(int((clamped + 60.0) * 10))
        self._level_meter.setFormat(f"{db:+.1f} dBFS")

    def _load_compensation(self):
        path, _ = QFileDialog.getOpenFileName(
            self,
            "Load Compensation File",
            "",
            "Compensation files (*.csv *.txt);;All files (*)",
        )
        if not path:
            return
        comp_changed = path != self._compensation_path
        self._compensation_path = path
        self._comp_label.setText(path.split("/")[-1])
        logger.info("Compensation file set: %s", path)
        if comp_changed and self._measurements_db:
            n_cleared = len(self._measurements_db)
            self._measurements_db.clear()
            self._again_btn.setEnabled(False)
            self._save_btn.setEnabled(False)
            self._fr_curve.setData([], [])
            self._avg_label.setText("Measurements averaged: 0")
            self._status_label.setText(
                f"Compensation changed — {n_cleared} prior measurement(s) discarded. Re-measure."
            )
            logger.info("Cleared %d stale measurements after comp change", n_cleared)

    def _start_measurement(self):
        """Begin a fresh measurement, discarding any prior averages."""
        if self._worker is not None and self._worker.isRunning():
            self._status_label.setText("Measurement already in progress…")
            return

        if self._measurements_db:
            n = len(self._measurements_db)
            self._measurements_db.clear()
            logger.info("Start Measurement: cleared %d prior measurement(s)", n)

        self._begin_measurement()

    def _measure_again(self):
        """Run another measurement and average it with the existing ones."""
        if self._worker is not None and self._worker.isRunning():
            self._status_label.setText("Measurement already in progress…")
            return
        self._begin_measurement()

    def _begin_measurement(self):
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
            normalize_to_reference_band,
        )

        self._current_ir = ir
        self._current_sr = sample_rate

        freqs, mag_db = compute_frequency_response(ir, sample_rate)

        def _at(f_hz: float, arr: np.ndarray) -> float:
            return float(arr[np.argmin(np.abs(freqs - f_hz))])

        logger.info(
            "FR raw (pre-comp): 50Hz=%+.1f, 200Hz=%+.1f, 500Hz=%+.1f, 1k=%+.1f, 5k=%+.1f, 10k=%+.1f dB",
            _at(50, mag_db), _at(200, mag_db), _at(500, mag_db),
            _at(1000, mag_db), _at(5000, mag_db), _at(10000, mag_db),
        )

        if self._compensation_path:
            from paraeq.measurement.compensation import (
                apply_compensation,
                load_compensation,
            )

            comp_freqs, comp_gains_db = load_compensation(self._compensation_path)
            mag_db = apply_compensation(mag_db, freqs, comp_freqs, comp_gains_db)
            logger.info("Compensation applied from %s", self._compensation_path)
            logger.info(
                "FR post-comp:    50Hz=%+.1f, 200Hz=%+.1f, 500Hz=%+.1f, 1k=%+.1f, 5k=%+.1f, 10k=%+.1f dB",
                _at(50, mag_db), _at(200, mag_db), _at(500, mag_db),
                _at(1000, mag_db), _at(5000, mag_db), _at(10000, mag_db),
            )

        mag_db = normalize_to_reference_band(freqs, mag_db, low_hz=200.0, high_hz=1000.0)
        logger.info(
            "FR normalized:   50Hz=%+.1f, 200Hz=%+.1f, 500Hz=%+.1f, 1k=%+.1f, 5k=%+.1f, 10k=%+.1f dB",
            _at(50, mag_db), _at(200, mag_db), _at(500, mag_db),
            _at(1000, mag_db), _at(5000, mag_db), _at(10000, mag_db),
        )
        mag_db_smooth = fractional_octave_smooth(mag_db, freqs, fraction=6)

        self._measurements_db.append(mag_db_smooth)
        avg_db = average_measurements(self._measurements_db)
        self._current_freqs = freqs

        # Update plot (skip DC bin)
        mask = freqs > 0
        self._fr_curve.setData(freqs[mask], avg_db[mask])
        plotted = avg_db[mask]
        finite = plotted[np.isfinite(plotted)]
        if finite.size:
            y_min, y_max = float(finite.min()), float(finite.max())
            pad = max(3.0, 0.05 * (y_max - y_min))
            self._plot_widget.setYRange(y_min - pad, y_max + pad)
            logger.info(
                "FR plot Y range set to (%.1f, %.1f) dB", y_min - pad, y_max + pad
            )

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
        model = self._name_edit.text() or "Unknown"
        self.save_profile_requested.emit(
            {"name": model, "ir": self._current_ir, "sample_rate": self._current_sr}
        )
