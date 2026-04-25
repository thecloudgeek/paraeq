"""Profile management for headphone correction configurations."""

import json
import shutil
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from platformdirs import user_data_dir


@dataclass
class Profile:
    name: str
    headphone_model: str
    target_curve_name: str = "flat"
    impulse_response: np.ndarray | None = field(default=None, repr=False)
    sample_rate: int = 48000
    fir_coefficients: np.ndarray | None = field(default=None, repr=False)
    eq_bands: list[dict] = field(default_factory=list)
    correction_mode: str = "fir"
    notes: str = ""

    def set_measurement(self, ir: np.ndarray, sample_rate: int):
        self.impulse_response = ir
        self.sample_rate = sample_rate

    def set_eq_bands(self, bands: list[dict]):
        self.eq_bands = bands

    def export_json(self, filepath: Path):
        data = {
            "name": self.name,
            "headphone_model": self.headphone_model,
            "target_curve_name": self.target_curve_name,
            "sample_rate": self.sample_rate,
            "eq_bands": self.eq_bands,
            "correction_mode": self.correction_mode,
            "notes": self.notes,
        }
        with open(filepath, "w") as f:
            json.dump(data, f, indent=2)

    @classmethod
    def import_json(cls, filepath: Path) -> "Profile":
        with open(filepath, "r") as f:
            data = json.load(f)
        p = cls(
            name=data["name"],
            headphone_model=data["headphone_model"],
            target_curve_name=data.get("target_curve_name", "flat"),
            sample_rate=data.get("sample_rate", 48000),
            correction_mode=data.get("correction_mode", "fir"),
            notes=data.get("notes", ""),
        )
        p.eq_bands = data.get("eq_bands", [])
        return p

    def save_ir_wav(self, filepath: Path):
        if self.impulse_response is None:
            return
        import scipy.io.wavfile as wav

        ir = self.impulse_response
        max_val = np.max(np.abs(ir))
        if max_val > 0:
            ir = ir / max_val
        wav.write(str(filepath), self.sample_rate, ir.astype(np.float32))

    def load_ir_wav(self, filepath: Path):
        import scipy.io.wavfile as wav

        sr, ir = wav.read(str(filepath))
        self.sample_rate = sr
        self.impulse_response = ir.astype(np.float64)


class ProfileManager:
    def __init__(self, profiles_dir: Path | None = None):
        if profiles_dir is None:
            profiles_dir = Path(user_data_dir("ParaEQ")) / "profiles"
        self.profiles_dir = profiles_dir
        self.profiles_dir.mkdir(parents=True, exist_ok=True)

    def save(self, profile: Profile):
        profile_dir = self.profiles_dir / profile.name
        profile_dir.mkdir(parents=True, exist_ok=True)
        profile.export_json(profile_dir / "profile.json")
        if profile.impulse_response is not None:
            profile.save_ir_wav(profile_dir / "impulse_response.wav")
        if profile.fir_coefficients is not None:
            np.save(profile_dir / "fir_coefficients.npy", profile.fir_coefficients)

    def load(self, name: str) -> Profile:
        profile_dir = self.profiles_dir / name
        profile = Profile.import_json(profile_dir / "profile.json")
        ir_path = profile_dir / "impulse_response.wav"
        if ir_path.exists():
            profile.load_ir_wav(ir_path)
        fir_path = profile_dir / "fir_coefficients.npy"
        if fir_path.exists():
            profile.fir_coefficients = np.load(fir_path)
        return profile

    def list_profiles(self) -> list[str]:
        names = []
        for p in sorted(self.profiles_dir.iterdir()):
            if p.is_dir() and (p / "profile.json").exists():
                names.append(p.name)
        return names

    def delete(self, name: str):
        profile_dir = self.profiles_dir / name
        if profile_dir.exists():
            shutil.rmtree(profile_dir)

    def duplicate(self, name: str, new_name: str):
        src = self.profiles_dir / name
        dst = self.profiles_dir / new_name
        shutil.copytree(src, dst)
        profile = self.load(new_name)
        profile.name = new_name
        self.save(profile)
