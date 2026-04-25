"""Real-time spectrum analyzer with pre/post correction traces."""
import logging
from collections import deque

import numpy as np
import pyqtgraph as pg
from PyQt6.QtCore import QTimer, pyqtSlot
from PyQt6.QtWidgets import (
    QComboBox,
    QHBoxLayout,
    QLabel,
    QSlider,
    QSizePolicy,
    QVBoxLayout,
    QWidget,
)
from PyQt6.QtCore import Qt

logger = logging.getLogger(__name__)

# FFT configuration
FFT_SIZE = 4096
SAMPLE_RATE = 48_000
OVERLAP = FFT_SIZE // 2        # 50 % overlap
UPDATE_INTERVAL_MS = 50        # ~20 fps

# Smoothing window sizes (in FFT bins, averaging)
_SMOOTHING_OPTIONS = {
    "None": 1,
    "1/3 oct": 3,
    "1/6 oct": 6,
    "1/12 oct": 12,
}

FREQS = np.fft.rfftfreq(FFT_SIZE, 1.0 / SAMPLE_RATE)
WINDOW = np.hanning(FFT_SIZE)


def _compute_magnitude_db(buf: np.ndarray) -> np.ndarray:
    """FFT of the most recent FFT_SIZE samples, returns magnitude in dB."""
    frame = buf[-FFT_SIZE:] if len(buf) >= FFT_SIZE else np.zeros(FFT_SIZE)
    windowed = frame[-FFT_SIZE:] * WINDOW
    spectrum = np.fft.rfft(windowed)
    magnitude = np.abs(spectrum) / (FFT_SIZE / 2)
    magnitude = np.maximum(magnitude, 1e-10)
    return 20.0 * np.log10(magnitude)


class SpectrumAnalyzer(QWidget):
    """Real-time spectrum analyzer: pre-correction (dim) + post-correction (bright)."""

    def __init__(self, parent=None):
        super().__init__(parent)

        # Circular buffers holding latest audio samples
        self._pre_buffer: deque = deque(maxlen=FFT_SIZE * 4)
        self._post_buffer: deque = deque(maxlen=FFT_SIZE * 4)

        # Latest computed spectra
        self._pre_db: np.ndarray = np.full(len(FREQS), -80.0)
        self._post_db: np.ndarray = np.full(len(FREQS), -80.0)

        self._setup_ui()
        self._start_timer()

    # ------------------------------------------------------------------
    # UI
    # ------------------------------------------------------------------

    def _setup_ui(self):
        root = QVBoxLayout(self)
        root.setSpacing(6)

        # --- Controls ---
        ctrl = QHBoxLayout()

        ctrl.addWidget(QLabel("Smoothing:"))
        self._smooth_combo = QComboBox()
        for label in _SMOOTHING_OPTIONS:
            self._smooth_combo.addItem(label)
        ctrl.addWidget(self._smooth_combo)

        ctrl.addWidget(QLabel("dB range:"))
        self._db_slider = QSlider(Qt.Orientation.Horizontal)
        self._db_slider.setRange(20, 120)
        self._db_slider.setValue(80)
        self._db_slider.setFixedWidth(120)
        self._db_slider.valueChanged.connect(self._on_db_range_changed)
        ctrl.addWidget(self._db_slider)

        self._db_range_label = QLabel("80 dB")
        ctrl.addWidget(self._db_range_label)

        ctrl.addStretch()
        root.addLayout(ctrl)

        # --- Plot ---
        self._plot_widget = pg.PlotWidget(title="Spectrum Analyzer")
        self._plot_widget.setLabel("left", "Magnitude (dB)")
        self._plot_widget.setLabel("bottom", "Frequency (Hz)")
        self._plot_widget.setLogMode(x=True, y=False)
        self._plot_widget.showGrid(x=True, y=True, alpha=0.3)
        self._plot_widget.setXRange(np.log10(20), np.log10(20_000))
        self._plot_widget.setYRange(-80, 0)
        self._plot_widget.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Expanding
        )
        self._plot_widget.addLegend()

        self._pre_curve = self._plot_widget.plot(
            pen=pg.mkPen("#78909C", width=1.5),  # dim bluish-grey
            name="Pre-correction",
        )
        self._post_curve = self._plot_widget.plot(
            pen=pg.mkPen("#4FC3F7", width=2),  # bright cyan
            name="Post-correction",
        )

        root.addWidget(self._plot_widget)

    def _start_timer(self):
        self._timer = QTimer(self)
        self._timer.setInterval(UPDATE_INTERVAL_MS)
        self._timer.timeout.connect(self._update_plot)
        self._timer.start()
        logger.debug("SpectrumAnalyzer timer started at %d ms", UPDATE_INTERVAL_MS)

    # ------------------------------------------------------------------
    # Public feed methods (called from audio engine callbacks)
    # ------------------------------------------------------------------

    @pyqtSlot(object)
    def feed_pre(self, block: np.ndarray):
        """Feed a block of pre-correction audio samples."""
        flat = np.asarray(block, dtype=np.float32).flatten()
        self._pre_buffer.extend(flat.tolist())

    @pyqtSlot(object)
    def feed_post(self, block: np.ndarray):
        """Feed a block of post-correction audio samples."""
        flat = np.asarray(block, dtype=np.float32).flatten()
        self._post_buffer.extend(flat.tolist())

    # ------------------------------------------------------------------
    # Slots
    # ------------------------------------------------------------------

    def _on_db_range_changed(self, value: int):
        self._db_range_label.setText(f"{value} dB")
        self._plot_widget.setYRange(-value, 0)

    def _update_plot(self):
        """Called by timer at ~20 fps to refresh the spectrum display."""
        mask = (FREQS >= 20) & (FREQS <= 20_000)
        freqs_visible = FREQS[mask]

        # Pre
        if len(self._pre_buffer) >= FFT_SIZE:
            pre_arr = np.array(self._pre_buffer)
            self._pre_db = _compute_magnitude_db(pre_arr)
        pre_visible = self._pre_db[mask]
        pre_visible = self._smooth(pre_visible)

        # Post
        if len(self._post_buffer) >= FFT_SIZE:
            post_arr = np.array(self._post_buffer)
            self._post_db = _compute_magnitude_db(post_arr)
        post_visible = self._post_db[mask]
        post_visible = self._smooth(post_visible)

        self._pre_curve.setData(freqs_visible, pre_visible)
        self._post_curve.setData(freqs_visible, post_visible)

    def _smooth(self, mag_db: np.ndarray) -> np.ndarray:
        """Apply simple box smoothing based on the combo selection."""
        label = self._smooth_combo.currentText()
        width = _SMOOTHING_OPTIONS.get(label, 1)
        if width <= 1:
            return mag_db
        kernel = np.ones(width) / width
        return np.convolve(mag_db, kernel, mode="same")
