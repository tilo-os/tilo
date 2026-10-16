#!/usr/bin/env python3

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


REPORTER = Path(__file__).resolve().parents[1] / "tilo-session-report.py"


def main() -> int:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "previous.log").write_text(
            "paint-stats: 4/7 painted | events=1 revision=0 anim=3 session=0 keepalive=0 | 144.027 Hz target | present=9 backpressure=12\n"
            "application stopped before presenting a window \x1b[3mpackage_id\x1b[0m\x1b[2m=\x1b[0mdev.tilo.terminal \x1b[3mdisplay_name\x1b[0m\x1b[2m=\x1b[0mTerminal \x1b[3mexit_code\x1b[0m\x1b[2m=\x1b[0mSome(1)\n"
            "Connected to Xwayland on :1\n"
            "tilo compositor: DRM scanout: modeset complete connector=42 width=2880 height=1800 refresh_mhz=144000\n"
            "frames=287 direct_frames=287 direct_dropped=3 flip_completions=286 unprofiled_frames=2 window_us=2000199 bridge_us=1700000 scanout_us=24000 readback_bytes=0 output_bytes=0 direct_scanout_status=enabled direct_scanout_reason=\"not-initialized\" DRM render phase summary\n"
            "direct_scanout_status=enabled renderer_pci=0000:66:00.0 drm_bus_id=\"pci:0000:66:00.0\" DRM direct scanout enabled\n"
            "package_id=chatgpt request_to_dispatch_us=1200 dispatching user-requested host application launch\n"
            "package_id=chatgpt pid=123 request_to_spawn_us=8200 application launcher spawned\n"
            "package_id=chatgpt surface_id=77 request_to_map_us=42000 spawn_to_map_us=33800 application toplevel mapped\n"
            "package_id=chatgpt surface_id=77 frame_seq=1 request_to_first_present_us=50000 spawn_to_first_present_us=41800 map_to_first_present_us=8000 application first frame presented\n"
            "paint-stats: 0/12025 painted | events=0 revision=0 anim=0 session=0 keepalive=0 | 144.027 Hz target | present=9 backpressure=12\n"
            "thread 'main' panicked at wayland-backend/src/rs/server_impl/common_poll.rs:284:38:\n"
            "tilo-flight-record-v1 status=134 logout_request=no\n"
        )
        (root / "previous-frame-trace.csv").write_text(
            "seq,start_us,period_us,input_us,pump_us,layout_us,scene_us,submit_us,testctl_us,clients_us,sleep_us,painted,present_async,input_latency_us,input_retirement_latency_us,backpressured,deadline_late_us\n"
            "0,0,0,1,2,3,4,5,0,6,7,1,1,8,9000,0,500\n"
            "1,20000,20000,2,3,4,5,6,0,7,8,1,1,20000,24000,1,3500\n"
        )
        output = root / "reports" / "incident.json"
        subprocess.run(
            [sys.executable, str(REPORTER), "--root", str(root), "--previous", "--output", str(output)],
            check=True,
        )
        report = json.loads(output.read_text())
        assert report["schema"] == "tilo-session-report-v1"
        assert report["flight"]["app_failures"] == {"dev.tilo.terminal": 1}
        assert report["flight"]["paint_last"]["backpressure"] == "12"
        assert report["flight"]["xwayland_ready"] is True
        assert report["flight"]["fatal_panic"] is True
        assert report["flight"]["idle_wake_storm_max_ticks"] == 12025
        assert report["flight"]["direct_scanout_enabled"] is True
        assert report["flight"]["direct_scanout_status_last"] == {
            "status": "enabled",
            "reason": None,
        }
        assert report["flight"]["direct_scanout_fallback_tail"] == []
        assert report["flight"]["render_phase_last"] == {
            "frames": 287,
            "direct_frames": 287,
            "direct_dropped": 3,
            "flip_completions": 286,
            "unprofiled_frames": 2,
            "window_us": 2000199,
            "bridge_us": 1700000,
            "scanout_us": 24000,
            "readback_bytes": 0,
            "output_bytes": 0,
        }
        assert report["flight"]["kms_modes"] == [{
            "connector": 42,
            "width": 2880,
            "height": 1800,
            "refresh_mhz": 144000,
        }]
        assert report["flight"]["launch_latency_us"]["chatgpt"] == {
            "map_to_first_present_us": {"count": 1, "p50": 8000, "p95": 8000, "max": 8000},
            "request_to_dispatch_us": {"count": 1, "p50": 1200, "p95": 1200, "max": 1200},
            "request_to_first_present_us": {"count": 1, "p50": 50000, "p95": 50000, "max": 50000},
            "request_to_map_us": {"count": 1, "p50": 42000, "p95": 42000, "max": 42000},
            "request_to_spawn_us": {"count": 1, "p50": 8200, "p95": 8200, "max": 8200},
            "spawn_to_first_present_us": {"count": 1, "p50": 41800, "p95": 41800, "max": 41800},
            "spawn_to_map_us": {"count": 1, "p50": 33800, "p95": 33800, "max": 33800},
        }
        assert report["frames"]["painted"] == 2
        assert report["frames"]["painted_fps"] == 100.0
        assert report["frames"]["input_latency_us"]["p99"] == 20000
        assert report["frames"]["input_retirement_latency_us"]["p99"] == 24000
        assert report["frames"]["backpressured_ticks"] == 1
        assert report["frames"]["deadline_late_us"]["p99"] == 3500
        assert report["frames"]["paint_period_us"]["p99"] == 20000
        assert output.stat().st_mode & 0o777 == 0o600

        def gate(log: str, *extra: str) -> subprocess.CompletedProcess[str]:
            (root / "latest.log").write_text(log)
            return subprocess.run(
                [sys.executable, str(REPORTER), "--root", str(root), "--render-gate", *extra],
                capture_output=True,
                text=True,
                check=False,
            )

        enabled = (
            "direct_scanout_status=enabled format=Xrgb8888 renderer_pci= earlier_failures=0 DRM direct scanout enabled\n"
        )
        degraded = (
            "direct_scanout_status=degraded direct_scanout_reason=\"render: GPU-only absolute deadline exceeded\" "
            "output=1 hdr=false failures=1 retry_in_ms=500 DRM direct scanout degraded to readback\n"
        )
        readback_summary = (
            "frames=40 direct_frames=0 direct_dropped=0 flip_completions=0 unprofiled_frames=0 window_us=2000000 "
            "bridge_us=1800000 scanout_us=3000 readback_bytes=866713600 output_bytes=866713600 avg_direct_us=0 "
            "max_direct_us=0 direct_failures=1 direct_scanout_status=unavailable "
            "direct_scanout_reason=\"render: GPU-only absolute deadline exceeded\" DRM render phase summary\n"
        )
        direct_summary = (
            "frames=120 direct_frames=120 direct_dropped=0 flip_completions=120 unprofiled_frames=0 window_us=2000000 "
            "bridge_us=0 scanout_us=0 readback_bytes=0 output_bytes=0 avg_direct_us=6100 max_direct_us=9800 "
            "direct_failures=1 direct_scanout_status=enabled direct_scanout_reason=\"not-initialized\" DRM render phase summary\n"
        )
        # Phase `a_demotion_that_never_recovers_fails_the_gate`.
        failed = gate(enabled + direct_summary + degraded + readback_summary)
        assert failed.returncode == 1, failed
        assert "direct GPU scanout is not presenting" in failed.stderr, failed.stderr
        assert "readback frames averaged 45000 us" in failed.stderr, failed.stderr
        # Phase `a_retried_demotion_passes_the_gate`.
        passed = gate(enabled + degraded + enabled + direct_summary)
        assert passed.returncode == 0, passed
        assert "render gate: PASS" in passed.stdout
        # Phase `direct_frames_over_budget_fail_the_gate`.
        slow = gate(enabled + direct_summary, "--render-budget-ms", "5")
        assert slow.returncode == 1 and "direct frames averaged 6100 us" in slow.stderr, slow
        # Phase `a_required_direct_presenter_must_start`.
        software = (
            "direct_scanout_status=unavailable reason=\"renderer and scanout use different GPUs\" DRM direct scanout unavailable\n"
            "frames=30 direct_frames=0 direct_dropped=0 flip_completions=0 unprofiled_frames=0 window_us=2000000 "
            "bridge_us=300000 scanout_us=3000 readback_bytes=1 output_bytes=1 direct_scanout_status=unavailable "
            "direct_scanout_reason=\"renderer and scanout use different GPUs\" DRM render phase summary\n"
        )
        assert gate(software).returncode == 0
        assert gate(software, "--require-direct").returncode == 1

        # Phase `typed_keys_aggregate_per_app`.
        (root / "latest.log").write_text(
            "KEY-LATENCY app=so.tilo.notes key_to_commit_us=9000 key_to_submit_us=10000 key_to_present_us=18000\n"
            "KEY-LATENCY app=so.tilo.notes key_to_commit_us=30000 key_to_submit_us=31000 key_to_present_us=50000\n"
            "KEY-LATENCY app=so.tilo.notes unanswered=1\n"
        )
        keys = subprocess.run(
            [sys.executable, str(REPORTER), "--root", str(root)],
            capture_output=True, text=True, check=True,
        )
        notes = json.loads(keys.stdout)["flight"]["key_latency_us"]["so.tilo.notes"]
        assert notes["unanswered"] == 1
        assert notes["key_to_present_us"] == {
            "count": 2, "p50": 18000, "p95": 50000, "p99": 50000, "max": 50000,
        }
        table = subprocess.run(
            [sys.executable, str(REPORTER), "--root", str(root), "--key-latency"],
            capture_output=True, text=True, check=True,
        ).stdout
        assert "so.tilo.notes: unanswered=1" in table and "p50=18.0ms" in table, table
    # A long session: the full record holds only measurement lines by now,
    # and the launch timing and failure live in the recorder's event record.
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "latest.log").write_text(
            "paint-stats: 3/14 painted | events=0 revision=0 anim=0 session=0 keepalive=0 | 60.000 Hz target | present=1 backpressure=0\n"
            * 64
        )
        (root / "events.log").write_text(
            "tilo-flight-record-v1 start=now pid=42 shell=/usr/bin/shell\n"
            "2026-10-11T02:22:31.204118Z  INFO tilo::launch_latency: package_id=com.onepassword.OnePassword pid=9 request_to_spawn_us=1200000 application launcher spawned\n"
            "2026-10-11T02:22:45.760719Z ERROR tilo::app_launch: application stopped before presenting a window package_id=com.onepassword.OnePassword display_name=1Password exit_code=None\n"
        )
        output = root / "current.json"
        subprocess.run(
            [sys.executable, str(REPORTER), "--root", str(root), "--output", str(output)],
            check=True,
        )
        flight = json.loads(output.read_text())["flight"]
        assert flight["app_failures"] == {"com.onepassword.OnePassword": 1}, flight["app_failures"]
        assert flight["launch_latency_us"]["com.onepassword.OnePassword"]["request_to_spawn_us"]["max"] == 1200000
        assert flight["paint_last"]["painted"] == "3"

    print("session report: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
