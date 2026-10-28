"""The hardware DRM checks must fail on missing evidence and pass only on the lines they name."""
import unittest

import drm_hardware_checks

ATOMIC = "tilo compositor: DRM scanout: kms_flip_api=atomic connector=41"
LEGACY = ("tilo compositor: DRM scanout: kms_flip_api=legacy "
          "reason=\"the connector presents without page flips\" connector=41")
LEGACY_FORCED = ("tilo compositor: DRM scanout: kms_flip_api=legacy "
                 "reason=\"forced by TILO_KMS_ATOMIC=0\" connector=41")
DEMOTED = ("tilo compositor: DRM scanout: kms_flip_api=legacy "
           "reason=\"an atomic flip failed: Invalid argument (os error 22)\" crtc=63 "
           "DRM page flips fall back to legacy")
PACING = ("FRAME-PACING retired=118 vblank_frames=118 missed=0 queue_depth=1 vblank_locked=true "
          "interval_us=16667 lead_us=4200 cost_p50_us=2400 cost_p99_us=5100 "
          "build_to_vblank_p50_us=7300 build_to_vblank_p99_us=9400")
PHASE = "frames=120 direct_frames=2 direct_dropped=0 flip_completions=118 client_scanout_frames=112"

ENV = {"commit": "c" * 40, "profile": "release", "renderer": "gpu", "virtualization": "none",
       "runner": "device-yoga", "hardware": "LENOVO 83ED", "started_at": "2026-10-10T01:15:00Z",
       "meta": {"image_digest": "sha256:" + "d" * 64}}


def graded(checks):
    return {item["check"]: item["status"] for item in checks.results if item["status"] != "report"}


class AtomicFlips(unittest.TestCase):
    def test_an_atomic_output_with_no_failure_passes(self):
        checks = drm_hardware_checks.Checks("\n".join(["receipt: x", ATOMIC]), ENV)
        checks.atomic_flips()
        self.assertEqual(graded(checks), {"atomic-flips": "pass"})

    def test_no_flip_line_at_all_fails_rather_than_skips(self):
        checks = drm_hardware_checks.Checks("receipt: x\n", ENV)
        checks.atomic_flips()
        self.assertEqual(graded(checks), {"atomic-flips": "fail"})

    def test_a_connector_that_never_flips_fails(self):
        checks = drm_hardware_checks.Checks(LEGACY, ENV)
        checks.atomic_flips()
        self.assertEqual(graded(checks), {"atomic-flips": "fail"})

    def test_a_failed_atomic_flip_fails_even_beside_an_atomic_neighbour(self):
        checks = drm_hardware_checks.Checks("\n".join([ATOMIC, DEMOTED]), ENV)
        checks.atomic_flips()
        self.assertEqual(graded(checks), {"atomic-flips": "fail"})

    def test_the_forced_legacy_session_is_a_separate_check(self):
        self.assertEqual(graded(drm_hardware_checks.legacy_forced_off(LEGACY_FORCED)),
                         {"atomic-disabled": "pass"})
        self.assertEqual(graded(drm_hardware_checks.legacy_forced_off(ATOMIC)),
                         {"atomic-disabled": "fail"})

    def test_a_neighbour_staying_atomic_is_only_proven_by_a_demotion(self):
        self.assertEqual(graded(drm_hardware_checks.atomic_survives_failure(ATOMIC)),
                         {"atomic-flips-follow-mode": "fail"})
        self.assertEqual(graded(drm_hardware_checks.atomic_survives_failure("\n".join([ATOMIC, DEMOTED]))),
                         {"atomic-flips-follow-mode": "pass"})


class ScanoutAndPacing(unittest.TestCase):
    def test_a_client_buffer_window_over_one_refresh_fails(self):
        text = "CLIENT-BUFFER-RELEASE releases=112 window=2000 p50_us=12400 p99_us=30100 max_us=40000"
        checks = drm_hardware_checks.Checks(text, ENV)
        checks.client_buffer_release(16_667)
        self.assertEqual(graded(checks), {"client-buffer-release": "pass"})
        checks = drm_hardware_checks.Checks("CLIENT-BUFFER-RELEASE p50_us=21000 window=2000", ENV)
        checks.client_buffer_release(16_667)
        self.assertEqual(graded(checks), {"client-buffer-release": "fail"})

    def test_no_release_line_fails(self):
        checks = drm_hardware_checks.Checks("receipt: x", ENV)
        checks.client_buffer_release(16_667)
        self.assertEqual(graded(checks), {"client-buffer-release": "fail"})

    def test_paced_frames_within_one_interval_pass(self):
        checks = drm_hardware_checks.Checks("\n".join([ATOMIC, PACING]), ENV)
        checks.vblank_pacing()
        self.assertEqual(graded(checks), {"vblank-pacing": "pass"})

    def test_an_unlocked_clock_or_a_late_flip_fails(self):
        unlocked = PACING.replace("vblank_locked=true", "vblank_locked=false")
        late = PACING.replace("build_to_vblank_p50_us=7300", "build_to_vblank_p50_us=18000")
        for text in (unlocked, late, "receipt: x"):
            checks = drm_hardware_checks.Checks(text, ENV)
            checks.vblank_pacing()
            self.assertEqual(graded(checks), {"vblank-pacing": "fail"}, text)

    def test_scanout_is_reported_and_never_grades(self):
        checks = drm_hardware_checks.Checks("client_scanout_status=active output=1", ENV)
        checks.client_scanout()
        self.assertEqual(checks.results[0]["status"], "report")
        self.assertEqual(checks.results[0]["detail"], "a fullscreen client reached scanout")

    def test_a_summary_field_block_is_read_as_one_line(self):
        wrapped = "\n".join([PHASE.replace(" flip_completions", "\nflip_completions")
                             .replace(" client_scanout", "\nclient_scanout"),
                             "DRM render phase summary"])
        merged = drm_hardware_checks.lines(wrapped, "client_scanout_frames=112")
        self.assertEqual(len(merged), 1)
        self.assertEqual(drm_hardware_checks.fields(merged[0])["client_scanout_frames"], "112")
        self.assertEqual(drm_hardware_checks.lines(wrapped, "client_scanout_status="), [])


if __name__ == "__main__":
    unittest.main()
