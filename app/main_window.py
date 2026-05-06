"""Main application window with tab-based navigation."""
import logging

import numpy as np
from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QLabel,
    QMainWindow,
    QStatusBar,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from paraeq.profiles.profile import Profile, ProfileManager

logger = logging.getLogger(__name__)


class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("ParaEQ")
        self.setMinimumSize(1024, 768)

        self._profile_manager = ProfileManager()
        self._active_profile: Profile | None = None
        self._audio_engine = None

        self._setup_ui()
        self._connect_signals()

    # ------------------------------------------------------------------
    # UI construction
    # ------------------------------------------------------------------

    def _setup_ui(self):
        central = QWidget()
        self.setCentralWidget(central)
        layout = QVBoxLayout(central)
        layout.setContentsMargins(4, 4, 4, 4)

        self._tabs = QTabWidget()
        layout.addWidget(self._tabs)

        # Build real widget tabs
        self._measurement_tab = self._make_measurement_tab()
        self._target_tab = self._make_target_tab()
        self._eq_tab = self._make_eq_tab()
        self._analyzer_tab = self._make_analyzer_tab()
        self._profiles_tab = self._make_profiles_tab()

        self._tabs.addTab(self._measurement_tab, "Measure")
        self._tabs.addTab(self._target_tab, "Target")
        self._tabs.addTab(self._eq_tab, "EQ")
        self._tabs.addTab(self._analyzer_tab, "Analyzer")
        self._tabs.addTab(self._profiles_tab, "Profiles")

        self._status_bar = QStatusBar()
        self.setStatusBar(self._status_bar)
        self._status_bar.showMessage("Ready")

    def _make_measurement_tab(self) -> QWidget:
        try:
            from app.wizard.measurement_wizard import MeasurementWizard

            return MeasurementWizard()
        except Exception as exc:
            logger.warning("Could not load MeasurementWizard: %s", exc)
            return self._placeholder("Measure")

    def _make_target_tab(self) -> QWidget:
        try:
            from app.editor.target_curve_editor import TargetCurveEditor

            return TargetCurveEditor()
        except Exception as exc:
            logger.warning("Could not load TargetCurveEditor: %s", exc)
            return self._placeholder("Target")

    def _make_eq_tab(self) -> QWidget:
        try:
            from app.eq.manual_eq_editor import ManualEQEditor

            return ManualEQEditor()
        except Exception as exc:
            logger.warning("Could not load ManualEQEditor: %s", exc)
            return self._placeholder("EQ")

    def _make_analyzer_tab(self) -> QWidget:
        try:
            from app.analyzer.spectrum_analyzer import SpectrumAnalyzer

            return SpectrumAnalyzer()
        except Exception as exc:
            logger.warning("Could not load SpectrumAnalyzer: %s", exc)
            return self._placeholder("Analyzer")

    def _make_profiles_tab(self) -> QWidget:
        try:
            from app.profiles.profile_manager import ProfileManagerWidget

            widget = ProfileManagerWidget()
            # Pre-populate with saved profiles
            for name in self._profile_manager.list_profiles():
                try:
                    profile = self._profile_manager.load(name)
                    widget.add_profile(profile)
                except Exception as exc:
                    logger.warning("Could not load profile '%s': %s", name, exc)
            return widget
        except Exception as exc:
            logger.warning("Could not load ProfileManagerWidget: %s", exc)
            return self._placeholder("Profiles")

    def _placeholder(self, text: str) -> QWidget:
        widget = QWidget()
        layout = QVBoxLayout(widget)
        label = QLabel(text)
        label.setAlignment(Qt.AlignmentFlag.AlignCenter)
        layout.addWidget(label)
        return widget

    # ------------------------------------------------------------------
    # Signal wiring
    # ------------------------------------------------------------------

    def _connect_signals(self):
        # Measurement → Target editor
        if hasattr(self._measurement_tab, "measurement_complete"):
            self._measurement_tab.measurement_complete.connect(
                self._on_measurement_complete
            )

        # Target editor → correction generated
        if hasattr(self._target_tab, "correction_generated"):
            self._target_tab.correction_generated.connect(
                self._on_correction_generated
            )

        # Manual EQ changed
        if hasattr(self._eq_tab, "eq_changed"):
            self._eq_tab.eq_changed.connect(self._on_eq_changed)

        # Profile activated
        if hasattr(self._profiles_tab, "profile_activated"):
            self._profiles_tab.profile_activated.connect(self._on_profile_activated)

    # ------------------------------------------------------------------
    # Slot handlers
    # ------------------------------------------------------------------

    def _on_measurement_complete(self, data: dict):
        """Route measurement data to the target editor."""
        logger.info(
            "Measurement complete: model=%s, freqs=%d",
            data.get("headphone_model", "unknown"),
            len(data.get("frequencies", [])),
        )
        self._status_bar.showMessage(
            f"Measurement complete: {data.get('headphone_model', 'unknown')}"
        )

        if hasattr(self._target_tab, "set_measurement"):
            self._target_tab.set_measurement(
                data["frequencies"], data["magnitude_db"]
            )
            self._tabs.setCurrentWidget(self._target_tab)

    def _on_correction_generated(self, payload):
        """Route generated correction to the appropriate downstream tab."""
        kind = payload.get("type")
        logger.info("Correction generated: type=%s", kind)

        if kind == "peq":
            bands = payload.get("data") or []
            if hasattr(self._eq_tab, "load_bands"):
                self._eq_tab.load_bands(bands)
                self._tabs.setCurrentWidget(self._eq_tab)
            self._status_bar.showMessage(f"Parametric EQ generated ({len(bands)} bands)")
        elif kind == "fir":
            self._status_bar.showMessage(
                f"FIR filter generated ({len(payload.get('data', []))} taps)"
            )
        else:
            self._status_bar.showMessage("Correction generated")

    def _on_eq_changed(self, bands):
        """Push updated EQ bands to IIR processor."""
        logger.info("EQ changed: %d bands", len(bands))
        self._status_bar.showMessage(f"EQ updated — {len(bands)} band(s)")
        self._update_iir_processor(bands)

    def _on_profile_activated(self, profile: Profile):
        """Load and apply the activated profile."""
        logger.info("Profile activated: %s", profile.name)
        self._active_profile = profile
        self._status_bar.showMessage(f"Profile: {profile.name}")

        if profile.eq_bands and hasattr(self._eq_tab, "load_bands"):
            from paraeq.correction.parametric_eq import EQBand

            bands = [
                EQBand(
                    filter_type=b["filter_type"],
                    fc=b["fc"],
                    gain_db=b["gain_db"],
                    q=b["q"],
                )
                for b in profile.eq_bands
            ]
            self._eq_tab.load_bands(bands)

    # ------------------------------------------------------------------
    # Audio engine
    # ------------------------------------------------------------------

    def _update_iir_processor(self, bands):
        """Forward new EQ bands to the running IIR processor."""
        if self._audio_engine is None:
            return
        try:
            from paraeq.correction.parametric_eq import ParametricEQ
            from paraeq.engine.iir_processor import IIRProcessor

            if bands:
                peq = ParametricEQ(bands, 48_000)
                sos = peq.combined_sos()
                processor = IIRProcessor(sos)
                self._audio_engine.set_processor(processor.process)
                logger.info("IIR processor updated with %d bands", len(bands))
            else:
                self._audio_engine.set_processor(None)
                logger.info("IIR processor cleared (no bands)")
        except Exception as exc:
            logger.error("Failed to update IIR processor: %s", exc)

    def start_audio_engine(self, output_device: int | None = None):
        """Start the real-time audio pass-through engine."""
        try:
            from paraeq.audio.devices import find_blackhole_device
            from paraeq.audio.stream import AudioPassThrough

            # Use BlackHole as the input (loopback) device if available
            blackhole = find_blackhole_device()
            input_device = blackhole.index if blackhole else output_device

            if input_device is None or output_device is None:
                logger.warning(
                    "Audio engine not started: input_device=%s, output_device=%s",
                    input_device,
                    output_device,
                )
                self._status_bar.showMessage(
                    "Audio engine not started — no devices configured"
                )
                return

            self._audio_engine = AudioPassThrough(
                input_device=input_device,
                output_device=output_device,
            )

            # Wire spectrum analyzer feeds
            if hasattr(self._analyzer_tab, "feed_pre"):
                self._audio_engine.set_pre_callback(self._analyzer_tab.feed_pre)
            if hasattr(self._analyzer_tab, "feed_post"):
                self._audio_engine.set_post_callback(self._analyzer_tab.feed_post)

            self._audio_engine.start()
            self._status_bar.showMessage("Audio engine running")
            logger.info(
                "Audio engine started: input=%s, output=%s",
                input_device,
                output_device,
            )
        except Exception as exc:
            logger.error("Failed to start audio engine: %s", exc)
            self._status_bar.showMessage(f"Audio engine error: {exc}")

    def stop_audio_engine(self):
        """Stop the real-time audio pass-through engine."""
        if self._audio_engine is not None:
            try:
                self._audio_engine.stop()
                logger.info("Audio engine stopped")
            except Exception as exc:
                logger.warning("Error stopping audio engine: %s", exc)
            self._audio_engine = None
        self._status_bar.showMessage("Audio engine stopped")

    def set_bypass(self, bypassed: bool):
        """Toggle audio engine bypass (EQ passthrough)."""
        if self._audio_engine is not None:
            self._audio_engine.set_bypass(bypassed)
            logger.info("Audio engine bypass=%s", bypassed)
        state = "bypassed" if bypassed else "active"
        self._status_bar.showMessage(f"EQ {state}")

    def activate_profile_by_name(self, name: str):
        """Look up a profile by name and activate it."""
        try:
            profile = self._profile_manager.load(name)
            self._on_profile_activated(profile)
            logger.info("Profile activated by name: %s", name)
        except Exception as exc:
            logger.error("Could not load profile '%s': %s", name, exc)

    def closeEvent(self, event):
        self.stop_audio_engine()
        super().closeEvent(event)
