"""First-launch setup wizard: BlackHole check, output device selection, done."""
import logging

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QComboBox,
    QLabel,
    QSizePolicy,
    QVBoxLayout,
    QWizard,
    QWizardPage,
)

logger = logging.getLogger(__name__)

PAGE_BLACKHOLE = 0
PAGE_OUTPUT = 1
PAGE_DONE = 2


# ---------------------------------------------------------------------------
# Pages
# ---------------------------------------------------------------------------


class BlackHolePage(QWizardPage):
    """Page 1: Check whether BlackHole is installed."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setTitle("Virtual Audio Driver Check")
        self.setSubTitle(
            "ParaEQ routes audio through BlackHole to apply real-time correction. "
            "This wizard checks whether it is installed."
        )

        layout = QVBoxLayout(self)
        self._status_label = QLabel("Checking for BlackHole…")
        self._status_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._status_label.setWordWrap(True)
        layout.addWidget(self._status_label)

        self._blackhole_device = None

    def initializePage(self):  # noqa: N802 — Qt override
        try:
            from paraeq.audio.devices import find_blackhole_device

            self._blackhole_device = find_blackhole_device()
        except Exception as exc:
            logger.warning("BlackHole detection error: %s", exc)
            self._blackhole_device = None

        if self._blackhole_device is not None:
            self._status_label.setText(
                f"BlackHole found: \"{self._blackhole_device.name}\"\n\n"
                "Click Next to continue."
            )
            logger.info(
                "BlackHole device found: %s (index=%d)",
                self._blackhole_device.name,
                self._blackhole_device.index,
            )
        else:
            self._status_label.setText(
                "BlackHole was NOT detected.\n\n"
                "ParaEQ can still run in stand-alone mode, but system-wide audio "
                "routing requires BlackHole to be installed.\n\n"
                "Download from: https://existential.audio/blackhole/\n\n"
                "Click Next to continue without BlackHole."
            )
            logger.warning("BlackHole device not found")


class OutputDevicePage(QWizardPage):
    """Page 2: Choose the physical output device (headphone amp / DAC)."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setTitle("Output Device Selection")
        self.setSubTitle(
            "Select the audio output device that your headphones or speakers are "
            "connected to."
        )

        layout = QVBoxLayout(self)
        layout.addWidget(QLabel("Output device:"))

        self._combo = QComboBox()
        self._combo.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed
        )
        layout.addWidget(self._combo)

        self._note = QLabel(
            "BlackHole and other virtual devices are hidden so you can pick "
            "your real hardware output."
        )
        self._note.setWordWrap(True)
        layout.addWidget(self._note)

        self._output_devices = []

    def initializePage(self):  # noqa: N802
        self._combo.clear()
        self._output_devices = []
        try:
            from paraeq.audio.devices import list_output_devices

            all_out = list_output_devices()
            # Exclude BlackHole virtual devices
            for dev in all_out:
                if "blackhole" not in dev.name.lower():
                    self._output_devices.append(dev)
                    self._combo.addItem(dev.name, userData=dev.index)
        except Exception as exc:
            logger.warning("Could not enumerate output devices: %s", exc)

        if not self._output_devices:
            self._combo.addItem("(no output devices found)")
            logger.warning("OutputDevicePage: no output devices found")
        else:
            logger.info(
                "OutputDevicePage: listed %d output devices", len(self._output_devices)
            )

    def selected_device_index(self) -> int | None:
        idx = self._combo.currentData()
        return idx if isinstance(idx, int) else None


class SetupCompletePage(QWizardPage):
    """Page 3: Setup complete."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setTitle("Setup Complete")
        self.setSubTitle(
            "ParaEQ is ready to use. You can change these settings later "
            "from the Preferences menu."
        )

        layout = QVBoxLayout(self)
        done_label = QLabel(
            "Click Finish to launch ParaEQ.\n\n"
            "Tip: use the Measure tab to take an impulse response measurement "
            "of your headphones."
        )
        done_label.setWordWrap(True)
        done_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(done_label)


# ---------------------------------------------------------------------------
# Wizard
# ---------------------------------------------------------------------------


class SetupWizard(QWizard):
    """Multi-page first-launch setup wizard."""

    def __init__(self, parent=None):
        super().__init__(parent)
        self.setWindowTitle("ParaEQ — First-Launch Setup")
        self.setMinimumSize(500, 380)
        self.setWizardStyle(QWizard.WizardStyle.ModernStyle)

        self._blackhole_page = BlackHolePage()
        self._output_page = OutputDevicePage()
        self._done_page = SetupCompletePage()

        self.addPage(self._blackhole_page)
        self.addPage(self._output_page)
        self.addPage(self._done_page)

        logger.info("SetupWizard created")

    @property
    def selected_output_device(self) -> int | None:
        """Return the user-selected output device index, or None."""
        return self._output_page.selected_device_index()
