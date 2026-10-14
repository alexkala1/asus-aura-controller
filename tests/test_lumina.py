import errno
import os
import subprocess
import sys
import tempfile
import time
import unittest
import unittest.mock
from unittest.mock import MagicMock, patch
import lumina_ctl

def _hold_lock(path):
    """Hold the Rust-compatible exclusive flock from a separate process."""
    code = ("import fcntl,sys,time;f=open(sys.argv[1],'r+b');"
            "fcntl.flock(f,fcntl.LOCK_EX);print('locked',flush=True);sys.stdin.readline()")
    proc = subprocess.Popen([sys.executable, "-I", "-c", code, path], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, text=True)
    assert proc.stdout.readline().strip() == "locked"
    return proc


class TestDeviceLock(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.NamedTemporaryFile()
        self.addCleanup(self.tmp.close)

    def test_gpu_open_blocks_ioctl_while_another_process_holds_the_lock(self):
        holder = _hold_lock(self.tmp.name)
        self.addCleanup(holder.kill)
        gpu = lumina_ctl.GpuAuraController(dev_path=self.tmp.name)
        with unittest.mock.patch.object(lumina_ctl, "DEVICE_LOCK_TIMEOUT", 0.15), \
                unittest.mock.patch("fcntl.ioctl") as ioctl:
            with self.assertRaises(TimeoutError) as ctx:
                gpu.open()
        ioctl.assert_not_called()
        self.assertIn(self.tmp.name, str(ctx.exception))
        self.assertIsNone(gpu.fd)  # descriptor closed on failure
        holder.stdin.write("\n"); holder.stdin.close(); holder.stdout.close(); holder.wait(5)
        with unittest.mock.patch("fcntl.ioctl") as ioctl:
            gpu.open()
        ioctl.assert_called_once()
        gpu.close()

    def test_mobo_open_waits_then_proceeds_after_release(self):
        holder = _hold_lock(self.tmp.name)
        self.addCleanup(holder.kill)
        mobo = lumina_ctl.MoboAuraController(port_path=self.tmp.name)
        with unittest.mock.patch.object(lumina_ctl, "DEVICE_LOCK_TIMEOUT", 0.15):
            with self.assertRaises(TimeoutError):
                mobo.open()
        self.assertIsNone(mobo.port_file)
        holder.stdin.write("\n"); holder.stdin.close(); holder.stdout.close(); holder.wait(5)
        mobo.open()
        self.assertIsNotNone(mobo.port_file)
        mobo.close()

    @staticmethod
    def _lock_is_free(path):
        import fcntl
        with open(path, "r+b") as other:
            try:
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError:
                return False
            return True

    def test_repeated_gpu_open_keeps_one_descriptor_and_one_ioctl(self):
        gpu = lumina_ctl.GpuAuraController(dev_path=self.tmp.name)
        with unittest.mock.patch("fcntl.ioctl") as ioctl:
            gpu.open()
            first = gpu.fd
            gpu.open()
        self.assertEqual(gpu.fd, first)
        ioctl.assert_called_once()
        self.assertFalse(self._lock_is_free(self.tmp.name))
        gpu.close()
        self.assertTrue(self._lock_is_free(self.tmp.name))
        with self.assertRaises(OSError):
            os.fstat(first)  # the one descriptor was really closed

    def test_public_methods_after_open_do_not_self_deadlock(self):
        gpu = lumina_ctl.GpuAuraController(dev_path=self.tmp.name)
        gpu.read_byte_data = unittest.mock.MagicMock(
            side_effect=[lumina_ctl.GPU_MAGIC_HIGH, lumina_ctl.GPU_MAGIC_LOW, 1, 2, 3, 1])
        started = time.monotonic()
        with unittest.mock.patch.object(lumina_ctl, "DEVICE_LOCK_TIMEOUT", 0.3), \
                unittest.mock.patch("fcntl.ioctl"):
            gpu.open()
            gpu.get_status()  # opens internally, then closes
        self.assertLess(time.monotonic() - started, 0.25)
        self.assertIsNone(gpu.fd)
        self.assertTrue(self._lock_is_free(self.tmp.name))

    def test_repeated_mobo_open_keeps_one_file_and_close_releases_the_lock(self):
        mobo = lumina_ctl.MoboAuraController(port_path=self.tmp.name)
        mobo.open()
        first = mobo.port_file
        mobo.open()
        self.assertIs(mobo.port_file, first)
        self.assertFalse(self._lock_is_free(self.tmp.name))
        mobo.close()
        self.assertTrue(first.closed)
        self.assertTrue(self._lock_is_free(self.tmp.name))

    def test_failed_second_open_never_loses_the_first_descriptor(self):
        gpu = lumina_ctl.GpuAuraController(dev_path=self.tmp.name)
        with unittest.mock.patch("fcntl.ioctl"):
            gpu.open()
            first = gpu.fd
            gpu.open()  # no-op, so no self-contention
        self.assertEqual(gpu.fd, first)
        gpu.close()

    def test_ioctl_failure_closes_the_descriptor(self):
        gpu = lumina_ctl.GpuAuraController(dev_path=self.tmp.name)
        with unittest.mock.patch("fcntl.ioctl", side_effect=OSError(errno.EIO, "boom")):
            with self.assertRaises(OSError):
                gpu.open()
        self.assertIsNone(gpu.fd)

    def test_dry_run_never_touches_device_files(self):
        missing = os.path.join(tempfile.gettempdir(), "aura-no-such-device")
        gpu = lumina_ctl.GpuAuraController(dev_path=missing, dry_run=True)
        mobo = lumina_ctl.MoboAuraController(dry_run=True, port_path=missing)
        gpu.open(); mobo.open()
        self.assertFalse(os.path.exists(missing))


class TestAuraController(unittest.TestCase):

    def test_parse_hex_color_valid(self):
        r, g, b = lumina_ctl.parse_hex_color("ff0055")
        self.assertEqual((r, g, b), (255, 0, 85))

        r, g, b = lumina_ctl.parse_hex_color("#00ffcc")
        self.assertEqual((r, g, b), (0, 255, 204))

    def test_parse_hex_color_invalid(self):
        with self.assertRaises(ValueError):
            lumina_ctl.parse_hex_color("xyz")
        with self.assertRaises(ValueError):
            lumina_ctl.parse_hex_color("12345")

    def test_gpu_dry_run_verification(self):
        gpu = lumina_ctl.GpuAuraController(dry_run=True)
        self.assertTrue(gpu.verify())
        status = gpu.get_status()
        self.assertEqual(status["address"], "0x29")
        self.assertTrue(status["magic_verified"])

    def test_gpu_safety_abort_on_bad_magic(self):
        gpu = lumina_ctl.GpuAuraController(dry_run=False)
        gpu.open = MagicMock()
        gpu.close = MagicMock()
        # Mock bad magic
        gpu.read_byte_data = MagicMock(side_effect=[0x00, 0x00])
        with self.assertRaises(RuntimeError) as ctx:
            gpu.set_color(255, 0, 0)
        self.assertIn("Safety abort", str(ctx.exception))

    def test_gpu_write_packet_sequence(self):
        gpu = lumina_ctl.GpuAuraController(dry_run=False)
        gpu.open = MagicMock()
        gpu.close = MagicMock()
        # Mock good magic
        gpu.read_byte_data = MagicMock(side_effect=[lumina_ctl.GPU_MAGIC_HIGH, lumina_ctl.GPU_MAGIC_LOW])
        written = []
        gpu.write_byte_data = MagicMock(side_effect=lambda reg, val: written.append((reg, val)))

        gpu.set_color(10, 20, 30, mode=1)
        self.assertEqual(written, [
            (lumina_ctl.GPU_REG_RED, 10),
            (lumina_ctl.GPU_REG_GREEN, 20),
            (lumina_ctl.GPU_REG_BLUE, 30),
            (lumina_ctl.GPU_REG_MODE, 1),
            (lumina_ctl.GPU_REG_APPLY, 0x01)
        ])

    def test_mobo_dry_run_verification(self):
        mobo = lumina_ctl.MoboAuraController(dry_run=True)
        self.assertEqual(mobo.verify(), lumina_ctl.MOBO_EXPECTED_NAME)
        status = mobo.get_status()
        self.assertEqual(status["slave_address"], "0x4e")
        self.assertEqual(status["firmware"], "LED-0116")
        self.assertEqual(len(status["zones"]), 5)

    def test_mobo_safety_abort_on_bad_firmware(self):
        mobo = lumina_ctl.MoboAuraController(dry_run=False)
        mobo.open = MagicMock()
        mobo.close = MagicMock()
        mobo.ene_read_reg = MagicMock(return_value=ord('X'))
        with self.assertRaises(RuntimeError) as ctx:
            mobo.set_mode(1)
        self.assertIn("Safety abort", str(ctx.exception))

    def test_mobo_color_order_rbg(self):
        mobo = lumina_ctl.MoboAuraController(dry_run=False)
        mobo.open = MagicMock()
        mobo.close = MagicMock()
        mobo.verify = MagicMock(return_value="LED-0116")
        writes = []
        mobo.ene_write_reg = MagicMock(side_effect=lambda reg, val: writes.append((reg, val)))

        # Set Red=255, Green=100, Blue=50
        mobo.set_color_static(255, 100, 50)
        # First LED direct color registers: 0x8000=R, 0x8001=B, 0x8002=G (ENE RBG quirk)
        self.assertIn((0x8000, 255), writes)
        self.assertIn((0x8001, 50), writes)
        self.assertIn((0x8002, 100), writes)
        # Apply register
        self.assertIn((0x80a0, 0x01), writes)


if __name__ == "__main__":
    unittest.main()


class TestCliParityWithRust(unittest.TestCase):
    """Dry-run only: the CLI is run as a subprocess, so no device file is ever opened."""

    @staticmethod
    def run_cli(*args):
        result = subprocess.run([sys.executable, "-I", os.path.join(os.path.dirname(__file__), "..", "lumina_ctl.py"),
                                 "--dry-run", *args], capture_output=True, text=True, timeout=20)
        writes = [line.split("Reg ", 1)[1] for line in result.stdout.splitlines() if "Write:" in line]
        return result, writes

    def test_all_off_needs_no_color(self):
        result, writes = self.run_cli("all", "--off")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("GPU and Motherboard LEDs turned off", result.stdout)

    def test_all_rainbow_needs_no_color_runs_gpu_spectrum_and_leaves_direct_mode(self):
        result, writes = self.run_cli("all", "--mode", "rainbow")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([w for w in writes if w.startswith("0x07") or w.startswith("0x0e")], ["0x07 <= 0x04", "0x0e <= 0x01"])
        mobo = [w for w in writes if w.startswith("0x80")]
        self.assertEqual(mobo, ["0x8020 <= 0x00", "0x8021 <= 0x05", "0x8022 <= 0x00", "0x8023 <= 0x00", "0x80a0 <= 0x01"])

    def test_static_still_requires_a_color_and_unknown_modes_are_errors(self):
        result, writes = self.run_cli("all")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--color is required for static mode", result.stderr)
        self.assertEqual(writes, [])
        result, writes = self.run_cli("all", "--color", "112233", "--mode", "bogus")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(writes, [])

    def test_color_driven_effect_writes_the_effect_buffer(self):
        result, writes = self.run_cli("mobo", "--color", "112233", "--mode", "breathing", "--speed", "2")
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = ["0x8020 <= 0x00"]
        for led in range(5):
            expected += [f"0x{0x8010 + 3 * led + o:04x} <= {b}" for o, b in enumerate(("0x11", "0x33", "0x22"))]
        expected += ["0x8021 <= 0x02", "0x8022 <= 0x02", "0x8023 <= 0x00", "0x80a0 <= 0x01"]
        self.assertEqual(writes, expected)

    def test_autonomous_effect_with_color_warns_and_writes_no_color_buffer(self):
        result, writes = self.run_cli("mobo", "--color", "112233", "--mode", "rainbow")
        self.assertIn("--color is ignored", result.stderr)
        self.assertFalse(any(w.startswith("0x8010") for w in writes))

    def test_aliases_and_gpu_rainbow(self):
        for args, first in [(("gpu", "--mode", "rainbow"), "0x07 <= 0x04"), (("gpu", "--mode", "strobing"), "0x07 <= 0x03"),
                            (("mobo", "--mode", "comet"), "0x8020 <= 0x00")]:
            result, writes = self.run_cli(*args)
            self.assertEqual(result.returncode, 0, (args, result.stderr))
            self.assertEqual(writes[0], first, args)
        result, writes = self.run_cli("mobo", "--mode", "comet")
        self.assertIn("0x8021 <= 0x07", writes)

    def test_mode_change_after_static_clears_direct_mode_first(self):
        mobo = lumina_ctl.MoboAuraController(dry_run=True)
        seen = []
        mobo.ene_write_reg = lambda reg, val: seen.append((reg, val))
        mobo.set_mode(lumina_ctl.MOBO_MODES["rainbow"], 1)
        self.assertEqual(seen[0], (lumina_ctl.MOBO_REG_DIRECT, 0))
        self.assertLess(seen.index((lumina_ctl.MOBO_REG_DIRECT, 0)), seen.index((lumina_ctl.MOBO_REG_APPLY, 1)))
        seen.clear()
        mobo.set_mode(lumina_ctl.MOBO_MODES["static"])  # static keeps the current buffer
        self.assertNotIn(lumina_ctl.MOBO_REG_DIRECT, [reg for reg, _ in seen])

    def test_status_reports_the_active_color_buffer(self):
        for direct, base, source in [(1, lumina_ctl.MOBO_REG_COLORS_DIRECT, "direct"), (0, lumina_ctl.MOBO_REG_COLORS_EFFECT, "effect")]:
            regs = {lumina_ctl.MOBO_REG_DIRECT: direct, lumina_ctl.MOBO_REG_CONFIG + 2: 5,
                    base: 0xaa, base + 1: 0xcc, base + 2: 0xbb}
            mobo = lumina_ctl.MoboAuraController(port_path=os.devnull)
            mobo.open = lambda: None
            mobo.close = lambda: None
            mobo.verify = lambda: "LED-0116"
            mobo.ene_read_reg = lambda reg, regs=regs: regs.get(reg, 0)
            status = mobo.get_status()
            self.assertEqual(status["color_source"], source)
            self.assertEqual(status["zones"][0]["hex"], "aabbcc")
