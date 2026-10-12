import unittest
from unittest.mock import MagicMock, patch
import aura_ctl

class TestAuraController(unittest.TestCase):

    def test_parse_hex_color_valid(self):
        r, g, b = aura_ctl.parse_hex_color("ff0055")
        self.assertEqual((r, g, b), (255, 0, 85))

        r, g, b = aura_ctl.parse_hex_color("#00ffcc")
        self.assertEqual((r, g, b), (0, 255, 204))

    def test_parse_hex_color_invalid(self):
        with self.assertRaises(ValueError):
            aura_ctl.parse_hex_color("xyz")
        with self.assertRaises(ValueError):
            aura_ctl.parse_hex_color("12345")

    def test_gpu_dry_run_verification(self):
        gpu = aura_ctl.GpuAuraController(dry_run=True)
        self.assertTrue(gpu.verify())
        status = gpu.get_status()
        self.assertEqual(status["address"], "0x29")
        self.assertTrue(status["magic_verified"])

    def test_gpu_safety_abort_on_bad_magic(self):
        gpu = aura_ctl.GpuAuraController(dry_run=False)
        gpu.open = MagicMock()
        gpu.close = MagicMock()
        # Mock bad magic
        gpu.read_byte_data = MagicMock(side_effect=[0x00, 0x00])
        with self.assertRaises(RuntimeError) as ctx:
            gpu.set_color(255, 0, 0)
        self.assertIn("Safety abort", str(ctx.exception))

    def test_gpu_write_packet_sequence(self):
        gpu = aura_ctl.GpuAuraController(dry_run=False)
        gpu.open = MagicMock()
        gpu.close = MagicMock()
        # Mock good magic
        gpu.read_byte_data = MagicMock(side_effect=[aura_ctl.GPU_MAGIC_HIGH, aura_ctl.GPU_MAGIC_LOW])
        written = []
        gpu.write_byte_data = MagicMock(side_effect=lambda reg, val: written.append((reg, val)))

        gpu.set_color(10, 20, 30, mode=1)
        self.assertEqual(written, [
            (aura_ctl.GPU_REG_RED, 10),
            (aura_ctl.GPU_REG_GREEN, 20),
            (aura_ctl.GPU_REG_BLUE, 30),
            (aura_ctl.GPU_REG_MODE, 1),
            (aura_ctl.GPU_REG_APPLY, 0x01)
        ])

    def test_mobo_dry_run_verification(self):
        mobo = aura_ctl.MoboAuraController(dry_run=True)
        self.assertEqual(mobo.verify(), aura_ctl.MOBO_EXPECTED_NAME)
        status = mobo.get_status()
        self.assertEqual(status["slave_address"], "0x4e")
        self.assertEqual(status["firmware"], "LED-0116")
        self.assertEqual(len(status["zones"]), 5)

    def test_mobo_safety_abort_on_bad_firmware(self):
        mobo = aura_ctl.MoboAuraController(dry_run=False)
        mobo.open = MagicMock()
        mobo.close = MagicMock()
        mobo.ene_read_reg = MagicMock(return_value=ord('X'))
        with self.assertRaises(RuntimeError) as ctx:
            mobo.set_mode(1)
        self.assertIn("Safety abort", str(ctx.exception))

    def test_mobo_color_order_rbg(self):
        mobo = aura_ctl.MoboAuraController(dry_run=False)
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
