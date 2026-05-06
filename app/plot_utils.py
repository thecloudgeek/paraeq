"""Shared pyqtgraph helpers for audio plots."""

import pyqtgraph as pg

# Standard audio FR plot tick frequencies (Hz). All other ticks render
# without labels so the axis stays readable when zoomed out.
_LABELED_FREQS = (20, 30, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000)


class AudioFreqAxis(pg.AxisItem):
    """Log-frequency axis with audio-conventional tick labels (20, 100, 1k, 10k…).

    Default pyqtgraph log-axis labels use scientific notation (2·10², 5·10²,
    etc.) and label every minor decade tick, which crowds the axis. This
    only labels the standard audio frequencies and formats values as Hz/kHz.
    """

    def tickStrings(self, values, scale, spacing):
        out = []
        for v in values:
            f = 10 ** v
            for std in _LABELED_FREQS:
                if abs(f - std) / std < 0.03:
                    out.append(f"{std}" if std < 1000 else f"{std // 1000}k")
                    break
            else:
                out.append("")
        return out
