"""ParaEQ application entry point."""
import logging
import sys
from pathlib import Path

from platformdirs import user_data_dir
from PyQt6.QtWidgets import QApplication

from app.main_window import MainWindow

logger = logging.getLogger(__name__)

SETUP_COMPLETE_FILE = Path(user_data_dir("ParaEQ")) / "setup_complete"


def main():
    app = QApplication(sys.argv)
    app.setApplicationName("ParaEQ")
    app.setOrganizationName("ParaEQ")

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s %(message)s",
    )

    window = MainWindow()

    # First-launch setup wizard
    selected_output_device: int | None = None
    if not SETUP_COMPLETE_FILE.exists():
        from app.setup.setup_wizard import SetupWizard

        wizard = SetupWizard(window)
        result = wizard.exec()
        if result:
            SETUP_COMPLETE_FILE.parent.mkdir(parents=True, exist_ok=True)
            SETUP_COMPLETE_FILE.touch()
            selected_output_device = wizard.selected_output_device
            logger.info(
                "Setup complete: output_device=%s, file=%s",
                selected_output_device,
                SETUP_COMPLETE_FILE,
            )
        else:
            logger.info("Setup wizard cancelled — proceeding anyway")

    window.show()

    # System tray
    tray = None
    try:
        from app.menu_bar.tray import SystemTray

        tray = SystemTray(app, window)

        # Wire bypass toggle → audio engine
        tray.set_callbacks(
            on_bypass_change=lambda bypassed: window.set_bypass(bypassed),
            on_profile_select=lambda name: window.activate_profile_by_name(name),
        )

        # Populate tray with existing profiles
        try:
            from paraeq.profiles.profile import ProfileManager

            pm = ProfileManager()
            tray.update_profiles(pm.list_profiles())
        except Exception as exc:
            logger.warning("Could not populate tray profiles: %s", exc)

        tray.show()
        window._tray = tray  # keep reference alive
    except Exception as exc:  # pragma: no cover
        logger.warning("Could not create system tray icon: %s", exc)

    # Start audio engine with the chosen output device
    window.start_audio_engine(output_device=selected_output_device)

    # Wire tray profile list to profile_activated signal
    if tray is not None and hasattr(window, "_profiles_tab"):
        profiles_tab = window._profiles_tab
        if hasattr(profiles_tab, "profile_activated"):
            profiles_tab.profile_activated.connect(
                lambda p: tray.set_active_profile(p.name)
            )

    sys.exit(app.exec())


if __name__ == "__main__":
    main()
