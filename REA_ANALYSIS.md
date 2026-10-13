# ASUS Aura Sync Reverse Engineering Analysis (REA) Dossier

> luminactl is an independent project, not affiliated with or endorsed by ASUSTeK Computer Inc. "ASUS", "Aura Sync" and "ROG" are ASUSTeK trademarks used only to describe the hardware protocols analyzed here. Register names such as `AURA_GPU_REG_*` are quoted from the analyzed vendor interfaces.

## 1. Upstream Binary Provenance & Triage

* **Source Package**: Official ASUS CDN Standalone Lighting Control Utility  
* **URL**: `https://dlcdnets.asus.com/pub/ASUS/mb/14Utilities/Lighting_Control_1.07.84_v2.zip`  
* **Package Size**: 207,956,594 bytes (~198 MiB)  
* **Local Extraction Path**: `upstream_binaries/extracted/` (git-ignored; never committed)

### Triage via REA (`rea-agents` & MCP)
Using REA (`inspect_managed_artifact`, `open_binary`, `binary_session`):
* `LightingService.exe`: PE32 x86 GUI, native (no CLI directory, not managed .NET). Coordinates IPC between system service and user session.
* `AacVgaHal_x64.dll` & `LED_DLL_forMB_x64.dll`: PE32+ x86_64 DLLs. Contain exported C APIs:
  - `AacHalDetect`
  - `fnEnumSupportDevice`
  - `fnSetLEDColor(device_ptr, zone, rgb_color, flags)`
  - `fnSetLEDEFFECT(device_ptr, zone, mode, flags)`
  - `fnSetLEDFrame`
* `Vender_x64.dll`: PE32+ x86_64 DLL providing direct hardware bus drivers:
  - `ReadI2C` (RVA 0x58260)
  - `WriteI2C` (RVA 0x59530)
  - `VGA_Read_I2C` (RVA 0x592a0)
  - `VGA_Write_I2C` (RVA 0x59500)
  - `VGA_SetLED_UingVender` (RVA 0x59460)
* `Hal/AXSP/AsIO.dll`: Direct Port I/O driver (`ASIO_InPortB`, `ASIO_OutPortB`, `GetPortVal`, `SetPortVal`). Used to communicate with the Super I/O chip.

---

## 2. Hardware Topology & Live Probes

### A. GPU: ASUS ROG Strix GeForce GTX 1060 6GB
* **Hardware ID**: `01:00.0 VGA compatible controller: NVIDIA Corporation GP106 [GeForce GTX 1060 6GB] (rev a1)`
* **Linux Interface**: Direct character device `/dev/i2c-0` (NVIDIA I2C adapter 1)
* **I2C Slave Address**: `0x29`
* **Magic Signature**:
  - Register `0x20`: `0x15`
  - Register `0x21`: `0x89`
  - *(Empirically verified on live host: returns `0x1589`)*
* **Register Map**:
  | Register | Name | Description |
  |---|---|---|
  | `0x04` | `AURA_GPU_REG_RED` | Red color byte (0x00 - 0xFF) |
  | `0x05` | `AURA_GPU_REG_GREEN` | Green color byte (0x00 - 0xFF) |
  | `0x06` | `AURA_GPU_REG_BLUE` | Blue color byte (0x00 - 0xFF) |
  | `0x07` | `AURA_GPU_REG_MODE` | Effect Mode (0=Off, 1=Static, 2=Breathing, 3=Flashing, 4=Spectrum Cycle, 5=Direct) |
  | `0x0C` | `AURA_GPU_REG_SYNC` | Synchronization control |
  | `0x0E` | `AURA_GPU_REG_APPLY` | Write `0x01` to commit / apply |
* **Empirical Live Read State**:
  - Red: `0x00`, Green: `0xd6`, Blue: `0xdc`, Mode: `0x01` (Static Teal/Cyan)

### B. Motherboard: ASUS ROG STRIX Z270E GAMING
* **Motherboard Model**: `STRIX Z270E GAMING`
* **Super I/O Controller**: Nuvoton `NCT6793D` at Super I/O ports `0x2e` (Index) and `0x2f` (Data).
* **Auxiliary SMBus Configuration**:
  - Logical Device (LD): `0x0B`
  - Base Address Register: `0x62`/`0x63` reads `0x02c0`
  - State: Activated (`register 0x30 = 0x01`)
* **Motherboard Aura Slave Address**: `0x4e` on SMBus port base `0x02c0`
* **Firmware / Chip Identification**:
  - Reading 16 bytes starting at register `0x1000` returns `b'LED-0116\x00\x00\x00\x00\x00\x00\x00\x00'` *(Empirically verified live)*
* **Port I/O Protocol (NCT6793D SMBus at 0x02c0)**:
  - `0x02c0` (`SMBHSTDAT`): Data register
  - `0x02c1` (`SMBBLKSZ`): Block size
  - `0x02c2` (`SMBHSTCMD`): Command type (`0x00` = Read Byte, `0x08` = Write Byte, `0x09` = Write Word)
  - `0x02c3` (`SMBHSTIDX`): SMBus command / register index
  - `0x02c4` (`SMBHSTCTL`): Control (`0x40` = Soft Reset, `0x80` = Manual Start)
  - `0x02c5` (`SMBHSTADD`): Target slave address (`(addr << 1) | read_write`)
  - `0x02c9` (`SMBHSTERR`): Error register (bit 5 `0x20` = NO_ACK)
  - `0x02ce` (`SMBHSTSTS`): Status register (bit 2 `0x04` = MANUAL_ACTIVE)
* **ENE Aura Register Framing over SMBus**:
  - Read: Write 16-bit swapped register address to index `0x00` via `WRITE_WORD`, then read byte from index `0x81` via `READ_BYTE`.
  - Write: Write 16-bit swapped register address to index `0x00` via `WRITE_WORD`, then write byte value to index `0x01` via `WRITE_BYTE`.
* **Key ENE Motherboard Registers**:
  | Register | Function |
  |---|---|
  | `0x1000` | Device string (16 bytes, `LED-0116`) |
  | `0x1c00` | Config table start (channel mappings) |
  | `0x8000` | Direct Mode color buffer (RGB per zone) |
  | `0x8010` | Internal Effect color buffer |
  | `0x8020` | Direct mode enable |
  | `0x8021` | Mode (see Full Mode Table below) |
  | `0x8022` | Speed (0x00=Fastest, 0x01=Fast, 0x02=Normal, 0x03=Slow, 0x04=Slowest) |
  | `0x8023` | Direction (0x00=Forward, 0x01=Reverse) |
  | `0x80a0` | Apply (`0x01`) / Save (`0xaa`) |

* **Full Motherboard Effect Mode Table**:
  | Mode ID | Name | Parameters |
  |---|---|---|
  | `0` | Off | None |
  | `1` | Static | Color buffer |
  | `2` | Breathing | Color buffer, Speed |
  | `3` | Flashing | Color buffer, Speed |
  | `4` | Spectrum Cycle | Speed |
  | `5` | Rainbow | Speed, Direction (Host board default) |
  | `6` | Rainbow Breathing | Speed |
  | `7` | Chase with Fade | Color, Speed, Direction |
  | `8` | Spectrum Chase with Fade | Speed, Direction |
  | `9` | Chase | Color, Speed, Direction |
  | `10` | Spectrum Chase | Speed, Direction |
  | `11` | Wave | Speed, Direction |
  | `12` | Rainbow Pulse | Speed |
  | `13` | Random Flicker | Speed |

* **Empirically Read 5-LED Hardware Mapping (STRIX Z270E GAMING)**:
  - Configuration bytes at `0x1c00`: `04 01 05 01 01 01 01 01 ... 85 85 85 85 86`
  - Total LED Count: `5`
  - LED 0..3: Channel `0x85` (`ENE_LED_CHANNEL_BACK_IO` - Back I/O Shroud)
  - LED 4: Channel `0x86` (`ENE_LED_CHANNEL_RGB_HEADER` - 12V 4-pin RGB Header 1)

* **CRITICAL Protocol Quirk: RBG Color Ordering**:
  - In `LED_DLL_forMB_x64.dll` and decompiled ENE HAL, direct color buffers (`0x8000 + i*3`) do NOT use standard RGB.
  - Byte 0: Red
  - Byte 1: Blue
  - Byte 2: Green
  - Writing standard RGB results in swapped green/blue output. Drivers must pack colors as `(R, B, G)`.

---

## 3. Disassembly & REA Cross-Reference Notes

1. **NvAPI Bypass on Linux**:
   On Windows, `LED_DLL_forMB_x64.dll` delegates to `Vender_x64.dll`, which loads `nvapi64.dll` and calls `NvAPI_I2CWriteEx` inside a user DX session. On Linux, this entire Windows IPC chain is unnecessary because the NVIDIA kernel driver exports `/dev/i2c-0` directly.
2. **Super I/O Port Locking**:
   On Linux, communicating with `0x02c0` requires raw port access (`/dev/port` with root/sudo or `iopl(3)` / `ioperm(3)`). Direct port access via `/dev/port` provides an immediate, zero-compilation user-space driver.
3. **Reference Controller Verification**:
   The standalone tool [lumina_ctl.py](lumina_ctl.py) implements both the GPU and Motherboard protocols, with safety signature verification and comprehensive unit tests.

