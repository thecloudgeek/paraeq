import json
import tempfile
from pathlib import Path
import numpy as np
from paraeq.profiles.profile import Profile, ProfileManager


def test_profile_creation():
    p = Profile(name="SE846 - Harman IE", headphone_model="Shure SE846", target_curve_name="harman_ie_2019")
    assert p.name == "SE846 - Harman IE"
    assert p.headphone_model == "Shure SE846"


def test_profile_set_measurement():
    p = Profile(name="test", headphone_model="test")
    ir = np.random.randn(4096, 2)
    p.set_measurement(ir, sample_rate=48000)
    assert p.impulse_response is not None
    np.testing.assert_array_equal(p.impulse_response, ir)
    assert p.sample_rate == 48000


def test_profile_set_eq_bands():
    p = Profile(name="test", headphone_model="test")
    bands_data = [
        {"filter_type": "peaking", "fc": 1000.0, "gain_db": 3.0, "q": 1.5},
        {"filter_type": "low_shelf", "fc": 100.0, "gain_db": 2.0, "q": 0.707},
    ]
    p.set_eq_bands(bands_data)
    assert len(p.eq_bands) == 2


def test_profile_export_import_json(tmp_path):
    p = Profile(name="SE846 - Harman IE", headphone_model="Shure SE846", target_curve_name="harman_ie_2019")
    p.set_eq_bands([{"filter_type": "peaking", "fc": 1000.0, "gain_db": 3.0, "q": 1.5}])
    filepath = tmp_path / "test_profile.json"
    p.export_json(filepath)
    loaded = Profile.import_json(filepath)
    assert loaded.name == "SE846 - Harman IE"
    assert loaded.headphone_model == "Shure SE846"
    assert len(loaded.eq_bands) == 1
    assert loaded.eq_bands[0]["fc"] == 1000.0


def test_profile_manager_save_load(tmp_path):
    manager = ProfileManager(profiles_dir=tmp_path)
    p = Profile(name="test_profile", headphone_model="Test HP")
    manager.save(p)
    loaded = manager.load("test_profile")
    assert loaded.name == "test_profile"


def test_profile_manager_list(tmp_path):
    manager = ProfileManager(profiles_dir=tmp_path)
    manager.save(Profile(name="alpha", headphone_model="A"))
    manager.save(Profile(name="beta", headphone_model="B"))
    names = manager.list_profiles()
    assert names == ["alpha", "beta"]


def test_profile_manager_delete(tmp_path):
    manager = ProfileManager(profiles_dir=tmp_path)
    manager.save(Profile(name="to_delete", headphone_model="X"))
    manager.delete("to_delete")
    assert "to_delete" not in manager.list_profiles()
