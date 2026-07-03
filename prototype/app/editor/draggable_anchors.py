"""Vertical-drag anchor overlay for the target curve editor.

A pyqtgraph GraphItem subclass that renders a row of control points at fixed
x-positions (log10(Hz)) and lets the user drag each one vertically. The plot
is in log-x mode, so positions are stored in log10-frequency space; only the
gain (y) coordinate changes on drag.
"""

import logging

import numpy as np
import pyqtgraph as pg
from pyqtgraph import QtCore

logger = logging.getLogger(__name__)


class DraggableAnchors(pg.GraphItem):
    """Fixed-frequency, vertically-draggable control points.

    Args:
        on_moved: callback ``(index: int, new_gain_db: float)`` invoked on each
            drag step with the anchor index and its new gain (y) value.
    """

    def __init__(self, on_moved):
        self._on_moved = on_moved
        self._log_freqs = np.zeros(0)
        self._gains = np.zeros(0)
        self._drag_index: int | None = None
        super().__init__()

    def set_anchors(self, freqs_hz: np.ndarray, gains_db: np.ndarray):
        """Position the anchors. ``freqs_hz`` are real Hz; stored as log10."""
        self._log_freqs = np.log10(np.asarray(freqs_hz, dtype=float))
        self._gains = np.asarray(gains_db, dtype=float).copy()
        self._redraw()

    def _redraw(self):
        if self._log_freqs.size == 0:
            self.setData()
            return
        pos = np.column_stack([self._log_freqs, self._gains])
        self.setData(
            pos=pos,
            size=12,
            symbol="o",
            symbolBrush=pg.mkBrush("#FFB74D"),
            symbolPen=pg.mkPen("#E65100", width=1.5),
            pxMode=True,
        )

    def mouseDragEvent(self, ev):
        if ev.button() != QtCore.Qt.MouseButton.LeftButton:
            ev.ignore()
            return
        if ev.isStart():
            pts = self.scatter.pointsAt(ev.buttonDownPos())
            if len(pts) == 0:
                self._drag_index = None
                ev.ignore()
                return
            self._drag_index = int(pts[0].index())
            ev.accept()
            return
        if self._drag_index is None:
            ev.ignore()
            return
        if ev.isFinish():
            self._drag_index = None
            ev.accept()
            return
        new_gain = float(ev.pos().y())
        self._gains[self._drag_index] = new_gain
        self._redraw()
        self._on_moved(self._drag_index, new_gain)
        ev.accept()
