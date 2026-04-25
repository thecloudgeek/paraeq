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
    if not SETUP_COMPLETE_FILE.exists():
        from app.setup.setup_wizard import SetupWizard

        wizard = SetupWizard(window)
        result = wizard.exec()
        if result:
            SETUP_COMPLETE_FILE.parent.mkdir(parents=True, exist_ok=True)
            SETUP_COMPLETE_FILE.touch()
            logger.info("Setup complete file created at %s", SETUP_COMPLETE_FILE)
        else:
            logger.info("Setup wizard cancelled — proceeding anyway")

    window.show()

    try:
        from app.menu_bar.tray import SystemTray

        tray = SystemTray(app, window)
        tray.show()
        window._tray = tray  # keep reference alive
    except Exception as exc:  # pragma: no cover
        logger.warning("Could not create system tray icon: %s", exc)

    sys.exit(app.exec())


if __name__ == "__main__":
    main()
