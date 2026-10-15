# Reverse-Engineering Differences: Vendor Aura Drivers vs. luminactl

Scope: static analysis (Ghidra 12.1.2 through REA, `rea-agents@6.3.0`) of the ASUS Lighting Control 1.07.84 binaries under `upstream_binaries/extracted/`, compared with `src/gpu.rs`, `src/mobo.rs`, `src/protocol.rs`.

> luminactl is an independent project, not affiliated with ASUSTeK Computer Inc. Function names below beginning `FUN_` are Ghidra auto-names; names such as `Set_LED_ExternalFan_Info_byCardIndex` are real PE exports.

**Evidence labels.** *Observed* = read directly from decompiled code/bytes. *Inferred* = reasoned from observed code. *Unknown* = not established. Nothing here was executed against hardware; static analysis does not show runtime behavior.

## 0. Executive summary

| # | Finding | Confidence | Risk to luminactl |
|---|---------|-----------|-------------------|
| G1 | Vendor live GPU writes (regs 4,5,6,7) **never write reg 0x0E**. 0x0E is only written by the explicit `fnSetLEDBurn` path. luminactl writes `0x0E=1` after every color/mode change. | Observed (code) / Inferred (that 0x0E persists to flash) | Possible flash wear / unintended persistence |
| G2 | Vendor translates mode `0` to `1` before writing reg 7 on the 0x52-address (our 0x29) path. luminactl writes mode `0`. | Observed | `off` may do something other than the vendor's behavior |
| G3 | Vendor dispatcher order is sync(0x0D) → mode(0x07) → burn(0x0E) → frame(0x0C) → color(0x04-06). luminactl writes color → mode → 0x0E. | Observed | Low (cosmetic ordering); see §2.4 |
| G4 | Vendor retries a failed NvAPI I2C write up to 3 more times with 50 ms sleeps. luminactl does not retry. | Observed | Low |
| G5 | Vendor addresses are stored as 8-bit (7-bit<<1): `0x52`/`0x54`/`0xCE`. luminactl's `0x29` is the `0x52` path. Consistent. | Observed | None |
| M1 | `Aac3572MbHal_x64.exe` contains **no `LED-0116` string and no 16-bit ENE register constants** (0x8020/0x8021/0x8022/0x8023/0x80A0/0x1C00). It targets "6K75 / ST9615 / STL452 / 6K582 / 3572" controllers and matches firmware names `AUMA0-E6K5` / `AUMA0-E8K4`. No vendor ground truth for the Z270E LED-0116 register sequence exists in the packaged binaries. | Observed (string scan + immediate scan; see §3.1) | Our mobo register map rests on live hardware reads and ENE-family analogy, not on this vendor HAL |
| M2 | Vendor discovers the SMBus base at runtime via Super I/O (unlock 0x87,0x87 → LDN 0x0B → regs 0x62/0x63 → relock 0xAA). luminactl hard-codes `0x02c0`. | Observed | Wrong base on other boards (safe on this one) |
| M3 | Vendor slave candidates are 8-bit `0x80` and `0x9C` (7-bit `0x40`, `0x4E`), probed by reading command `0xC1`. luminactl uses fixed `0x4E` ✓. | Observed | None |
| M4 | The vendor's persistent-save sequence is `0x80A0←0xAA` then `0x80F1←1` (after `0x80A0←1`). luminactl documents `0xAA` but never issues save/`0x80F1`. | Observed (ENE-GPU path + mobo `SetStandbyEffect`) | Feature gap, not a bug |

## 1. Tool provenance and verified addresses

| Binary | SHA-256 | Role |
|--------|---------|------|
| `vga_binaries/VGA_Extra_x64.dll` | `852eae26…c9865b` | GPU register writer (dispatcher + helpers) |
| `vga_binaries/LED_DLL_forMB_x64.dll` | `fbb5a606…099` | Marshals `fnSetLED*` calls to VGA_Extra through shared-memory mappings |
| `mb_binaries/Aac3572MbHal_x64.exe` | `afe45831…ce3f` | Motherboard COM HAL (SMBus/NCT Super I/O) |

Verified function addresses (all resolved in Ghidra, image base `0x180000000` / `0x140000000`):

| Address | Name | Evidence ID |
|---------|------|-------------|
| `VGA_Extra_x64.dll!0x180004a30` | `FUN_180004a30` master dispatcher | `ev_e8c0e887…abbc` |
| `0x180004c00` | `FUN_180004c00` mode writer | `ev_f0309e7b…3838` |
| `0x1800051a0` | `FUN_1800051a0` RGB writer | `ev_1b90322c…b22d` |
| `0x180004dc0` | `FUN_180004dc0` burn handler | `ev_e9b1ca3c…f46d` |
| `0x180005850` | `FUN_180005850` low-level I2C write (NvAPI/ADL) | `ev_1a85d7cc…a7` |
| `0x180003680` | `Set_LED_ExternalFan_Info_byCardIndex` (export; copies a 0x80-byte struct and calls the dispatcher) | `ev_7ce9dc32…31de6` |
| `0x180004150` | `FUN_180004150` register getter (reads regs 0x0E, 0x0B, 0x2F-0x32, 0x09) | `ev_b50f7d06…6dfe1ca` |
| `LED_DLL_forMB_x64.dll!0x18000bff0 / 0x18000c270 / 0x18000c980` | `fnSetLEDEFFECT` / `fnSetLEDColor` / `fnSetLEDBurn` | `ev_9205b52a…5a53` |
| `Aac3572MbHal_x64.exe!0x140027190` | NCTSmbus constructor (Super I/O discovery) | `ev_4751d845…f6a9` |
| `0x1400c1630` | `gs::NCTSmbus` vftable (read from ctor `lea`) | `ev_3cc3c39e…3c3f` |
| `0x140027390 / 0x1400274f0 / 0x140027650 / 0x1400277f0 / 0x140027990 / 0x140027af0` | vtbl[1..6]: write-byte / read-byte / write-word / read-word / read-block-ish / write-block | `ev_fa1cb214…b445b54` |
| `0x14002e230` | `GetMasterControllerAddress` | `ev_3de42478…6c4bbf` |
| `0x140036630` | `MyAacLedDevice::SetStandbyEffect` | `ev_08f92523…6f102` |

(Evidence IDs are abbreviated; the full `ev_<64 hex>` values were returned by the REA calls in this session.)

## 2. GPU: ROG Strix GTX 1060 (I2C 0x29)

### 2.1 Device-class table (observed)

`FUN_180004150`/`FUN_180004a30` index a per-card table at `DAT_180281e50` whose first byte is an **8-bit I2C address** (7-bit address << 1), or `1` for AMD/ADL:

| Table value | 7-bit | Path |
|-------------|-------|------|
| `0x52` | `0x29` | classic register map (**our GPU**) |
| `0x54` | `0x2A` | classic map + extra reg `0x09` write (dispatcher tail, `*piVar4 == 0x54`) |
| `0xCE` | `0x67` | ENE-style 16-bit register GPU (V2 buffers `0x8100/0x8160`) |
| `1` | n/a | callback `DAT_180282638` (AMD) |

`FUN_180005850` sends `[reg][data]` (`local_a8[0]=param_2`, `local_84=param_1`) through NvAPI when the vendor ID is `0x10de` and ADL when `0x1002`, and **retries**:

```c
if ((DAT_180282a00 == 0) || (cVar1 = (*DAT_180282a00)(&local_58,&local_98), cVar1 == '\0')) {
  uVar2 = 0;
  do {
    cVar1 = (*DAT_180282a00)(&local_58,&local_98);
    Sleep(0x32);                       /* 50 ms */
    if (cVar1 != '\0') break;
    uVar2 = uVar2 + 1;
  } while (uVar2 < 3);
}
```

### 2.2 Master dispatcher `FUN_180004a30` (observed)

`param_2` is an array of `int` {flag,value} pairs (the 0x80-byte struct copied by `Set_LED_ExternalFan_Info_byCardIndex`):

```c
if (param_2[0]    == 1) FUN_180005850(addr, 0x0d, &param_2[1],  card);   // reg 0x0D
if (param_2[0xc]  == 1) FUN_180004c00(card, param_2[0xd], led);          // mode (reg 7)
if (param_2[0xe]  == 1) FUN_180004dc0(card, param_2[0xf], led);          // burn (reg 0x0E)
if (param_2[0x10] == 1) FUN_180005850(addr, 0x0c, &param_2[0x11], card); // reg 0x0C
if (param_2[6]    == 1) FUN_1800051a0(card, &param_2[6..9], led);        // RGB
if (param_2[0x12] == 1 && addr == 0x54) FUN_180005850(addr, 9, &param_2[0x13], card); // reg 0x09
```

Each section runs only if its own flag is set. A "set color" call therefore writes only regs 4/5/6; the burn section is gated by `param_2[0xe]`, which only `fnSetLEDBurn` sets (`uStack_d0 = 1` at struct offset 0x38 in `LED_DLL_forMB_x64.dll!0x18000c980`).

### 2.3 Mode writer `FUN_180004c00`, classic branch (observed)

```c
else {                               /* address != 0xCE: our 0x52 path */
    pbVar7 = local_res10;            /* &mode */
    uVar5  = 7;                      /* register 7 */
    if (param_2 == 0) { local_res10[0] = 1; }   /* mode 0 -> 1 */
    uVar4 = *(undefined1 *)puVar2;   /* device address */
}
FUN_180005850(uVar4, uVar5, pbVar7, uVar6);
```

Only reg 7 is written. Mode `0` is **never written** on this path.

### 2.4 RGB writer `FUN_1800051a0`, classic branch (observed)

```c
local_b8[0] = (char)uStack_a4; FUN_180005850(addr, 4, local_b8, card);  // R
local_b8[0] = (char)uStack_a0; FUN_180005850(addr, 5, local_b8, card);  // G
local_b8[0] = (char)uStack_9c; FUN_180005850(addr, 6, local_b8, card);  // B
```

R,G,B order matches `GPU_REG_RED/GREEN/BLUE`. No apply write follows.

### 2.5 Burn handler `FUN_180004dc0` (observed)

```c
if (addr == 0xCE) {                      /* ENE-style GPU */
    reg 0x80A0 <- 1;  Sleep(0x32);       /* 50 ms */
    reg 0x80A0 <- 0xAA;
    reg 0x80F1 <- 1;
} else {
    FUN_180005850(addr, 0x0E, &burn_value, card);   /* classic: reg 0x0E <- caller value */
}
```

On the classic path the value written to 0x0E is whatever the caller supplies (`fnSetLEDBurn(…, param_2)`); `0x0E` is the **burn** register (name taken from the vendor export `fnSetLEDBurn`). The register-getter `FUN_180004150` reads `0x0E` back when `param_2+0x38 == 1`.

### 2.6 Side-by-side

| Aspect | Vendor (`VGA_Extra_x64.dll`) | luminactl (`src/gpu.rs`) | Verdict |
|--------|------------------------------|--------------------------|---------|
| Address | `0x52` (8-bit) = 7-bit `0x29` | `GPU_I2C_ADDR = 0x29` | ✅ match |
| Colour regs | `4=R, 5=G, 6=B` | same | ✅ match |
| Mode reg | `7`, mode `0` rewritten to `1` | `7`, writes `0` for `off` | ⚠️ differs (G2) |
| Mode/colour order | sync 0x0D, mode 7, burn 0x0E, frame 0x0C, colour | colour, mode, `0x0E=1` | ⚠️ differs (G3) |
| Reg 0x0E | only on explicit burn call | `GPU_REG_APPLY`, written after **every** `set_color`, `set_mode`, `restore` | ⚠️ differs, riskier (G1) |
| Regs 0x0C / 0x0D | written when caller sets flag | never written | gap (unknown semantics) |
| Reg 0x09 | only for `0x54` boards | never | n/a for 0x52 |
| Retry | up to 3 more attempts, 50 ms sleep | none | gap (G4) |
| Signature (0x20/0x21 = 0x1589) | not present in the four functions analysed | required before every op | luminactl is stricter; not contradicted (**Unknown** whether the vendor checks elsewhere) |

### 2.7 Recommendations (GPU)

1. **Stop writing `0x0E` on normal operations** (`set_color`, `set_mode`, `restore`). Hypothesis from vendor code: live updates need only regs 4/5/6/7. Gate `0x0E` behind an explicit `gpu burn` command if persistence is wanted. *Needs a hardware A/B test* (set colour without `0x0E`, confirm the card updates) before shipping, because static analysis cannot prove that this card latches 4-7 without a commit write. Tests that assert `[(4,..),(5,..),(6,..),(7,..),(0xE,1)]` (`src/gpu.rs` tests `set_color_write_order`, `set_mode_write_order_and_reverifies`) would change.
2. **Implement `off` as vendor does:** write mode `1` (static), with colour `0,0,0` supplied by the caller layer. Do not write mode `0` to reg 7 on this card until a hardware test shows what `0` does. `GpuMode::Off` should map to `(R,G,B)=(0,0,0)` + mode `1`.
3. Optionally reorder writes to `mode(7)` then `colour(4,5,6)` only if hardware testing shows a visible transient; no evidence of a functional requirement.
4. Add a bounded retry (≤3 attempts, 50 ms apart) on `EIO/ENXIO` from the I2C ioctl to mirror `FUN_180005850`. Keep the signature check.
5. Treat regs `0x0C`/`0x0D` as **unknown**. Do not write them without a hardware probe; `REA_ANALYSIS.md` currently calls 0x0C "Synchronization control" without vendor evidence for its meaning (only that it can be written when `param_2[0x10]==1`).

## 3. Motherboard: ENE LED-0116 on NCT6793D SMBus (0x02c0, slave 0x4e)

### 3.1 What the vendor HAL does and does not contain (observed)

* `strings`/Ghidra string search of `Aac3572MbHal_x64.exe` finds `AUMA0-E6K5` (file offset 786272) and `AUMA0-E8K4` (786304) but **no `LED-0116`**; neither UTF-16 nor ASCII `LED-0116` appears anywhere under `upstream_binaries/` (`grep -rla`).
* A scan of the executable for imm32/imm16/byte-swapped-imm16 forms of `0x8020, 0x8021, 0x8022, 0x8023, 0x80A0, 0x1C00` found none in code (the `0x8000`/`0x1000` hits were MSVC/CRT constants, e.g. `4881fa00100000` stack-probe compares and `VirtualFree` flag `0x8000`).
* Log strings name the controller families it creates: `6K75`, `ST9615`, `STL452`, `6K582 or 3572`, `ST072`, LED matrix. Device classes (RTTI): `MyAacLedDevice`, `…_Aura50`, `…_ADDR_STRIP(_Aura50[_V19])`, `…_L_STRIP_Aura50`, `…_LED_MATRIX`.
* Its register accessor takes an **8-bit command byte** (`(*vtbl+0x10)(obj, 0x25, effectId)`, `(…, 0xA0, 0xAA)`, `(…, 0xF1, 1)`), not the 16-bit index/data framing luminactl uses.

**Conclusion:** the Z270E LED-0116 sequence (`0x8020…0x80A0`, `0x1C00` config table, `0x1000` name) is **not verifiable from the packaged vendor mobo binary**. luminactl's register map is supported by live hardware reads (`REA_ANALYSIS.md`) and by ENE-family consistency with the GPU `0xCE` path (§3.5), not by this HAL. Treat the mobo mode IDs 5–13 and the `uses_color()` table as hardware-verified-only/unverified-by-vendor.

### 3.2 SMBus base discovery: NCTSmbus constructor `FUN_140027190` (observed)

```c
*param_1 = gs::NCTSmbus::vftable;
param_1[2] = CreateEventW(&sa, 0, 1, L"Global\\ASUS_SIO_MUTEX");
WaitForSingleObject((HANDLE)param_1[2], 1000);
outb(0x2e, 0x87); outb(0x2e, 0x87);       // enter extended function mode
outb(0x2e, 0x07); outb(0x2f, 0x0b);       // select logical device 0x0B
outb(0x2e, 0x62); hi = inb(0x2f);         // base high
outb(0x2e, 0x63); lo = inb(0x2f);         // base low
outb(0x2e, 0xaa);                          // exit extended function mode
SetEvent(...);
*(ushort *)(param_1 + 1) = (hi << 8) | lo; // SMBus I/O base cached in object+8
```

### 3.3 SMBus transaction framing, vtbl at `0x1400c1630` (observed)

Register offsets from base: `+0 DAT`, `+1 BLKSZ`, `+2 CMD-type`, `+3 IDX`, `+4 CTL`, `+5 ADD`, `+0xE STS`.

| vtbl slot | Op | Sequence |
|-----------|----|----------|
| 1 `0x140027390` | write byte | `CTL←0x40; DAT←val; ADD←addr; IDX←cmd; CMD←0x08; CTL←0x80`; poll `STS&4` ≤ 4000 reads |
| 2 `0x1400274f0` | read byte | `CTL←0x40; ADD←addr; IDX←cmd; CMD←0x00; CTL←0x80`; poll; `DAT` |
| 3 `0x140027650` | write word | `CTL←0x40; (extra port read); DAT←hi; DAT←lo; ADD←addr; IDX←cmd; CMD←0x09; CTL←0x80`; poll |
| 4 `0x1400277f0` | read word | `…CMD←0x01…`; reads `DAT` twice |
| 5/6 | block read / write | `CMD←0x02` / `CMD←0x0A`, `BLKSZ←len` |

Every operation is wrapped in `WaitForSingleObject/SetEvent` on `Global\ASUS_SIO_MUTEX` (and the HAL separately uses `Global\ASUS_AURA_SMBUS_MUTEX`, referenced from `0x140023985`).

### 3.4 Master/slave discovery `FUN_14002e230` (observed)

```c
for (i = 0; i < 2; i++) {
    obj = FUN_14002dfe0(DAT_1400bde34[i]);                 // candidate address
    ok  = (*vtbl_read_byte)(obj, 0xC1, &val);              // read command 0xC1
    if (ok && val != 0) return DAT_1400bde34[i];
}
```

Bytes at `0x1400bde34` are `80 9c` → 8-bit addresses `0x80` (7-bit `0x40`) and `0x9c` (7-bit **`0x4E`**).

### 3.5 ENE-style GPU path as ENE-family evidence (observed, applies to the GPU not the mobo)

`FUN_180004c00`/`FUN_1800051a0`/`FUN_180004dc0` for address `0xCE` use the same 16-bit register index framing and register names as luminactl, with V2 buffers:

```c
// RGB writer: effect buffer 0x8160 (or direct 0x8100 when "off"), 90 bytes in 3-byte cycles
local_68 = {0x81, flag==1 ? 0x00 : 0x60};  FUN_180005850(0xCE, 0, local_68, ...);  // index 0x8100/0x8160
local_b4[0] = R; local_b4[1] = B; local_b4[2] = G;       // R,B,G order
FUN_180005850(0xCE, 3, local_68, ...);                   // 0x0C-byte block writes, 0x5A bytes total
// then apply:  index 0x802F <- 1
// mode writer: index 0x8020 (word via cmd 2): {direct=0, mode}; "off" => {direct=1, mode=1} + black
```

* The `R,B,G` byte order independently confirms luminactl's `pack_mobo_color` (`[r, b, g]`).
* "Off" on ENE-type devices is **static + direct buffer + black**, not mode 0 (`local_res8[0] = 0x101`).
* Apply on V2 is `0x802F←1`; luminactl uses the V1 apply `0x80A0←1`. V1 vs V2 is a per-firmware difference, not a bug; LED-0116 uses V1 per live reads.
* Save: `0x80A0←1`, 50 ms sleep, `0x80A0←0xAA`, `0x80F1←1`.

### 3.6 Mobo "standby/save" sequence `MyAacLedDevice::SetStandbyEffect`, `0x140036630` (observed)

```c
(**(obj->vtbl + 0x10))(obj, 0x25, effectId & 0xff);   // cmd 0x25 <- effect
...
(**(obj->vtbl + 0x10))(obj, 0xA0, 0xAA);              // cmd 0xA0 <- 0xAA  (save)
if ((**(obj->vtbl + 0x68))() == 1) Sleep(0x14);       // 20 ms conditional
(**(obj->vtbl + 0x10))(obj, 0xF1, 1);                 // cmd 0xF1 <- 1
```

Inference: 8-bit commands `0xA0`/`0xF1` alias the `0x80A0`/`0x80F1` high-page registers used on the ENE GPU path. Not proven.

### 3.7 Side-by-side

| Aspect | Vendor | luminactl (`src/mobo.rs`) | Verdict |
|--------|--------|---------------------------|---------|
| Base discovery | SIO `0x2e/0x2f`, LDN `0x0B`, regs `0x62/0x63`, relock `0xAA` | hard-coded `0x02c0` | ⚠️ M2 |
| Slave | candidates `0x40`, `0x4E`, probe via cmd `0xC1` | fixed `0x4E` | ✅ correct, no probe |
| Write byte order | `CTL=0x40, DAT, ADD, IDX, CMD, CTL=0x80` | `CTL=0x40, ADD, IDX, DAT, CMD, CTL=0x80` | ✅ equivalent (`DAT` loaded before `CMD`/start in both) |
| Write word | `DAT←hi, DAT←lo`, CMD `0x09` | `DAT←low(swapped)=reg_hi, DAT←reg_lo`, CMD `0x09` | ✅ same wire bytes |
| Read byte / CMD codes | `0x00` read, `0x08` write byte, `0x09` word | same | ✅ |
| Busy poll | spins ≤4000 `STS` reads, no sleep | ≤150 polls, 1 ms sleeps | ✅ OK (ours is slower, bounded) |
| NO_ACK (`ERR` 0x9 bit5) | not checked in vtbl slots 1-3 | checked | ✅ ours is stricter |
| Cross-process lock | named event `Global\ASUS_SIO_MUTEX` | `hardware_lock` flock | ⚠️ cannot interoperate with a running Windows tool; irrelevant on Linux |
| Colour packing | R,B,G (GPU ENE path) | `[r,b,g]` | ✅ confirmed |
| Off | ENE-path: direct + static + black | `mode 0` with direct cleared | ⚠️ differs (see §3.8) |
| Apply | V1 `0x80A0←1` (analog) | `0x80A0←1` | ✅ consistent |
| Save | `0x80A0←0xAA`, `0x80F1←1` | not implemented | gap (M4) |
| Name check `LED-0116` | not in vendor HAL | enforced | ✅ safe (stricter), vendor cannot confirm |

### 3.8 Recommendations (mobo)

1. **Do not claim vendor verification for LED-0116 registers.** Update `REA_ANALYSIS.md` §2B/§3 wording: the mode table (IDs 5–13), speed/direction semantics and `0x1C00` layout are live-hardware/ENE-family derived; the packaged HAL speaks the 6K582/3572 8-bit command protocol.
2. **`off` robustness:** consider implementing `off` as the ENE-vendor does (direct=1, mode=1, colours `0,0,0`) as an alternative path and A/B test on the live board. `MoboMode::Off = 0` is accepted by ENE firmware in other implementations but is unproven here. Keep the current behavior until tested.
3. **Optional hardening:** read SIO regs `0x62/0x63` (LDN `0x0B`) and warn when the base ≠ `0x02c0` (vendor derives it dynamically; hard-coding is safe only on this board). Keep the existing LED-0116 name gate; it prevents misprogramming other firmware (e.g. `AUMA0-E6K5/E8K4`).
4. **Persistence (optional new feature):** if "save to flash" is desired, mimic the vendor's `0x80A0←1`, sleep 50 ms, `0x80A0←0xAA`, `0x80F1←1`. Gate it behind an explicit command and a confirmation, since flash wear and irreversible changes apply.
5. **No change needed** to SMBus framing (`0x40/0x80` control, `0x00/0x01` index/data, `CMD 0x08/0x09/0x00`): matches the vendor transport byte for byte.

## 4. Coverage and unknowns

| Question | Status |
|----------|--------|
| GPU dispatcher/mode/RGB/burn code paths (classic and ENE) | answered (observed) |
| Values the vendor passes for regs `0x0C`/`0x0D` and their meaning | **unknown** (only the write path was found; callers live in `AacVgaHal_x64.dll`, not analysed) |
| Whether reg `0x0E` is needed for the colour to latch on a Strix 1060, or what mode `0` does | **unknown** (needs a hardware test) |
| Mobo ENE register writes for `0x8020…0x80A0` in the vendor HAL | **not present** in `Aac3572MbHal_x64.exe`; other HALs (`Hal/AXSP/AsIO.dll`, `MBIsSupported.dll`) were checked only by strings: `AsIO.dll` exports port/MSR/memory primitives (`ASIO_InPortB/OutPortB/InPortD/MapMem/…`); `MBIsSupported.dll` reads CPUID/SMBIOS (`GenuineIntel`, `_SM_`) as a platform gate, not a protocol |
| `Vender_x64.dll` NvAPI/ADL transport | confirmed to import NvAPI/ADL I2C (`ADL2_Display_WriteAndReadI2C`, …); not decompiled |
| 0x2e/0x2f Super-I/O `LED-0116` read path on Z270E | luminactl-only (live read); no vendor counterpart |

Bounded negative searches: the `LED-0116` string search covered every file under `upstream_binaries/` (ASCII and UTF-16); the register-constant scan covered the `.text` sections of `Aac3572MbHal_x64.exe`, `LED_DLL_forMB_x64.dll`, `Vender_x64.dll`, `AacVgaHal_x64.dll`, `LightingService.exe`, `aaHMLib.dll`, `MBIsSupported.dll` and found no ENE register immediates, with two caveats: `LED_DLL_forMB_x64.dll` and `Vender_x64.dll` each have one `mov r12d, 0x8020` hit that was not examined (likely MFC/CRT flag code, unverified), and `VGA_Extra_x64.dll` was not immediate-scanned because its `0xCE` path was read directly from the decompilation (§3.5).
