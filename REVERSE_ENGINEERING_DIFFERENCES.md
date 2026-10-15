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

> **Phase 2 update.** The `0x0C`/`0x0D` "callers not analysed" unknown above is now partly answered: `AacVgaHal_x64.dll` has no call site for `fnSetLEDFrame` (§5.2). The `0x8020` hits in `LED_DLL_forMB_x64.dll`/`Vender_x64.dll` remain unexamined. See §5–§7.

## 5. GPU effects: speed, direction and frame sync (`AacVgaHal_x64.dll`)

Phase 2 method: Ghidra 12.1.2 (REA 6.3.0 for the first xref/loader reads, then Ghidra headless scripts after the REA MCP server dropped mid-session). Binaries (sha256 prefix): `AacVgaHal_x64.dll c1732b2b…`, `LED_DLL_forMB_x64.dll fbb5a606…`, `LED_DLL_forMB_User_x64.dll 23d648c3…`, `VGA_Extra_x64.dll 852eae26…`. Evidence IDs from the REA session: loader `ev_8059ca16…`, xrefs `ev_23836b04…`, `ev_a21466a9…`.

### 5.1 The call chain is not what the task assumed (observed)

`AacVgaHal_x64.dll` does **not** talk to `VGA_Extra_x64.dll`. `fn*` names are not PE exports of `AacVgaHal`; they are `GetProcAddress` strings. `DllInitialize` (`FUN_180015ec0`) loads **`LED_DLL_forMB_x64.dll`** and fills a function table:

```c
hModule = LoadLibraryA("LED_DLL_forMB_x64.dll");           // this+0x98
this+0x08 fnEnumSupportDevice   this+0x10 fnSetLEDColor   this+0x18 fnSetAddrLEDColor
this+0x20 fnSetLEDEFFECT        this+0x28 fnEffectReset    this+0x30 fnSetLEDFrame
this+0x38 fnGetLEDFrame         this+0x40 fnSetLEDBurn     this+0x78 fnGetLEDDeviceSpeedLevel
this+0x80 fnGetLEDDeviceGenerations   this+0x88 fnSetLEDDeviceEffectSpeed   this+0x90 fnSetLEDDeviceEffectDirection
// init succeeds only if ALL of them resolve and fnEnumSupportDevice() != 0
```

Full chain (each hop observed):

```
AacVgaHal_x64.dll  --GetProcAddress-->  LED_DLL_forMB_x64.dll!fn*          (service side)
  user-session (DAT_1802e0d22==0): jump through fn-pointers into LED_DLL_forMB_User_x64.dll
  service mode  (DAT_1802e0d22!=0): CreateFileMappingW("Global\SetLEDFrameDataMappingObject_x64" etc.,
                                     0x30/0x34 bytes) + SetEvent/WaitForSingleObject  -> AacVga_UserApp_x64.exe
LED_DLL_forMB_User_x64.dll (FUN_180002720) --LoadLibraryW("VGA_Extra_x64.dll")-->
     Set_LED_ExternalFan_Info_byCardIndex / Get_LED_ExternalFan_Info_byCardIndex   (generic 0x80-byte struct, §2.2)
     SetLEDDeviceEffectSpeed / SetLEDDeviceEffectDirection / GetLEDDeviceGenerations / GetLEDDeviceSpeedLevel
```

So `fnSetLEDFrame`, `fnEffectReset`, `fnSetLEDBurn` and `fnSetLEDEFFECT` all funnel into the **same** generic setter `Set_LED_ExternalFan_Info_byCardIndex` (`DAT_180282f38`) and `fnGetLEDFrame`/`GetLEDColor`/`GetLEDEFFECT`/`GetLEDBurn` into the same getter (`DAT_180282f40`). Speed, direction, generation and speed-level have their own `VGA_Extra_x64.dll` exports.

### 5.2 `fnSetLEDFrame` / `fnGetLEDFrame`: register `0x0C` (observed)

`LED_DLL_forMB_User_x64.dll!fnSetLEDFrame` (`0x180005160`):

```c
memset(&buf, 0, 0x80);                 // the 0x80-byte struct of §2.2 (int[32])
buf[0x10] = 1;                         // byte offset 0x40: "frame" flag
buf[0x11] = (u8)frame;                 // byte offset 0x44: value
if (id.dword1 == 0 && id.dword2 == 0)  // device id sanity gate (param_1[0..1] hi words must be 0)
    Set_LED_ExternalFan_Info_byCardIndex(card = param_1+0xc, &buf);
```

In the dispatcher (§2.2) `param_2[0x10]==1` → `FUN_180005850(addr, 0x0C, &param_2[0x11], card)`. So **`fnSetLEDFrame(frame)` writes register `0x0C` ← `frame` (one byte), via the same I2C path, no other register**. `fnEffectReset` is the sibling: it sets `buf[0] = 1` (flag) and `buf[1] = 1` (data: `local_88 = CONCAT44(uStack_104, 1)` with `uStack_104 = 1`), i.e. dispatcher `param_2[0]==1` → **register `0x0D` ← 1** (`FUN_180005850(addr, 0x0d, &param_2[1], card)`). Same register the dispatcher labels `0x0D` in §2.2 (the 'sync' write).

`fnGetLEDFrame` sets `buf[0x10]=1` and reads back `buf[0x11]`. In `Get_LED_ExternalFan_Info_byCardIndex` the `+0x40` flag reads **register `0x0B`**, not `0x0C` (`FUN_1800056b0(addr, 0x0b, …)`, I2C read, 5 attempts × 20 ms). That is an observed read/write asymmetry (`0x0C` write, `0x0B` read); whether `0x0B` mirrors `0x0C` on hardware is **unknown**.

Parameters seen at the exported ABI (`LED_DLL_forMB_x64.dll!fnSetLEDFrame`, `0x18000c5e0`): `(DeviceId* p1, u32 frame)`. `DeviceId` is 0x28 bytes; the dword at `+0xC` is the card index; the first dword pair must be zero. In service mode the 0x30-byte shared-memory block is `{u32 1; 0x28-byte DeviceId copy; u32 frame@+0x2c}` and the user app replays it.

**Callers: none.** `AacVgaHal_x64.dll` stores `fnSetLEDFrame`/`fnGetLEDFrame` (`this+0x30/0x38`) and only null-checks them; the only indirect calls through the table are `fnSetLEDColor` (`this+0x10`, an unnamed function near `0x1800164xx`) and `fnSetLEDEFFECT` (`this+0x20`, `FUN_1800164e0`). **No value for `0x0C` is ever chosen inside the HAL**, so what the frame byte means (the existing doc calls it "Synchronization control") is still not established by vendor code. Another consumer (e.g. `LightingService.exe`, not analysed) would have to supply it.

### 5.3 Speed and direction (observed, `VGA_Extra_x64.dll`)

```c
// SetLEDDeviceEffectSpeed(card, value, apply)         @0x180003cc0
// SetLEDDeviceEffectDirection(card, value, apply)     @0x180003da0
if (cardAddr[card] == 0xCE) {                 // ENE-type GPU only
    index  <- 0x8022 (speed)  | 0x8023 (direction)   // bytes {0x80,0x22}/{0x80,0x23}, cmd 0 = set index
    write8 <- value                                    // cmd 1
    if (apply == 1) { index <- 0x802F; write8 <- 1 }   // V2 apply
}                                              // else: NO-OP (returns without any write)
```

* **Only class `0xCE` (7-bit `0x67`) does anything.** For `0x52` (our 7-bit `0x29` Strix 1060), `0x54`, `0xC0` and AMD the functions are silent no-ops. Register `0x08`..`0x0F` classic regs have **no** speed or direction field in vendor code.
* The ENE GPU registers are the *same* `0x8022`/`0x8023` luminactl uses on the motherboard. Together with §3.5 this confirms the ENE register layout `0x8020 direct, 0x8021 mode, 0x8022 speed, 0x8023 direction, 0x802F/0x80A0 apply` as an ENE-family property (inferred for LED-0116).
* Value mapping is **not** done in this layer; the raw byte goes straight to the register. `0-2` for speed and direction come from the ENE HAL layer (§6.3), not from the GPU DLLs.

Capability queries (exact tables):

| `GetLEDDeviceGenerations` | class | `*out` |
|---|---|---|
| | `1`(AMD) or `0xC0` | 1 |
| | `0x52`, `0x54` | 2 |
| | `0xCE` | 3 |
| | other | 0 |

| `GetLEDDeviceSpeedLevel` | class | `*out` |
|---|---|---|
| | `0x54`, `0xC0`, `0xCE` | 3 |
| | `0x52`, `1`, other | **1** |

`AacVgaHal_x64.dll` consumes only the speed level: `MyAacLedDeviceHal::CreateLedDevice` (`FUN_180010f30`) calls `fnGetLEDDeviceSpeedLevel` per card, logs `... ledCount = %d, SpeedLevel = %d`, and stores it clamped to `1..0x21` (`0` → `1`). Level 1 means "no selectable speed". **Our Strix 1060 (`0x52`) therefore reports 1 speed level, generation 2, and has no vendor GPU speed or direction control at all.** `fnSetLEDDeviceEffectSpeed`/`Direction` and `fnGetLEDDeviceGenerations` have no call sites in the HAL (resolved and null-checked only).

### 5.4 Compare with `src/gpu.rs`

| Aspect | Vendor | luminactl | Verdict |
|--------|--------|-----------|---------|
| GPU speed/direction on `0x52` | not implemented (silent no-op); speed level reported = 1 | not implemented | ✅ nothing to port |
| GPU speed/direction on `0xCE` | `0x8022`/`0x8023` ← value, optional `0x802F ← 1` | n/a (no ENE GPU) | out of scope for this card |
| Frame reg `0x0C` | write `0x0C ← frame`; read `0x0B` | never touched | gap, **semantics unknown**, no caller in HAL |
| `fnEffectReset` | `0x0D ← 1` | never touched | gap, unknown effect |
| Generations | 2 for `0x52` | none | informational only |
| Retry | writes: 3×, 50 ms (`FUN_180005850`); reads: 5×, 20 ms (`FUN_1800056b0`) | none | add retry (see §2.7 item 4) |

## 6. Official ENE Technology HAL (`ene_asm_hal/HAL_DLL_X64`)

### 6.1 What the binary is (observed)

`HAL_DLL_X64` (sha256 `506b9e40…`) is `AacHal_x64.dll`, PDB `D:\Work\work_2020_Q1\1_Sam\asm_1095\output\Release\x64\AacHal_x64.pdb`, signed `ENE TECHNOLOGY INC.`, COM name `ENE_EHD_ASM.Hal`, log prefix `[AURA HAL][ENE ASM]`, registry key `Software\ENE_RGB_HAL_EHD_ASM`. `x64hash.hash` pairs it with `asmtusb_x64.dll` (ASMedia's USB bridge library; `EHD_CTRL_ASMEDIA_DLL_x64` is the `Asmt*` USB-lib wrapper exporting `AsmtUSBLib_*`).

**It is a USB-bridge LED controller HAL, not an SMBus driver.** There is no SMBus/SIO code, no `LED-0116`, no `0x1000..0x1007` name read, no `0x1C00` config read. Its only transport is one function:

```c
// FUN_180008600(dev, regIndex16, data*, len)        @0x180008600
local_47 = regIndex & 0xff;  local_48 = regIndex >> 8;  local_3f = len << 8;  local_78 = 0x53;   // 'S'
AsmtUSBLib_VENDOR_CMD(devHandle, 1 /*write*/, 0xEC, 0x41, ...);                // SCSI vendor cmd 0xEC, sub 0x41
```

`AacLedDeviceHalFactory` (COM `IClassFactory`), `MyAacLedDeviceHal` (enumerates devices through `AsmtUSBLib_vScanUSBPort`/`ScanEHD`), `MyAacLedDevice` (capabilities + colour/effect methods) and `IAacLedDeviceOptSpeed` (RTTI `.?AVIAacLedDeviceOptSpeed@@`) are present. Observed only by static reading, never run.

### 6.2 Register map the binary actually contains

| Immediate | Where | Meaning in this HAL |
|-----------|-------|--------------------|
| `0x8020` | `FUN_180008490` | 4-byte block `{direct, mode, speed, direction}` written in one transaction |
| `0x8021`/`0x8022`/`0x8023` | same 4-byte block, bytes 1/2/3 | mode / speed / direction (**inferred from the block layout**, not separate writes) |
| `0x802F` | `FUN_180008490` | apply: `← 1` after every update (V2 apply) |
| `0x8100` / `0x8160` | `FUN_180009170` | colour buffer: `0x8100` direct (`param_3==0`, effect "Static"), `0x8160` effect; up to 0x18 bytes in the vendor write |
| `0x80F2` | `FUN_180008580` | single byte, written from `FUN_18000a9d0`; unknown meaning (see 6.4) |
| `0x80A0` | **absent** | luminactl's V1 apply is not used; this HAL is V2 |
| `0x1C00` | **absent** | the only `0x1c00` hit is a CRT `memset` loop bound (`ene_all` line 12573) |
| `0x1000` | **absent** | only `0x1000` is a CRT buffer size / `%s` buffer; no name read |

So the "16-bit ENE register mapping" **is** partially present, but for the ASMedia/V2 layout (`0x8020…0x8023`, `0x802F`, `0x8100/0x8160`), not the V1 layout luminactl drives (`0x8000/0x8010` colour buffers, `0x80A0` apply, `0x1000` name, `0x1C00` config).

Colour packing (`FUN_180009170`): input stride 4, bytes copied `out[0]=in[0]; out[2]=in[1]; out[1]=in[2]` → **R,B,G** order, same as luminactl's `pack_mobo_color` `[r,b,g]`.

### 6.3 Effect IDs, speed and direction (observed)

`FUN_18001ca50` (static initialiser; constructor `FUN_18000ac50(name, id, color?, speed?, dir?, maxSpeed, …, maxDirection)`) builds the effect table. Flags decoded from the constructor arguments (the `colour` column is the `+0x51` byte; **inferred** meaning "caller supplies colours"):

| ID | Name | colour | speed | direction | max-dir |
|----|------|:------:|:-----:|:---------:|:-------:|
| 0 | Static | ✓ | – | – | – |
| 2 | Breathing | ✓ | ✓ | – | – |
| 3 | Strobing | ✓ | ✓ | – | – |
| 4 | Color cycle | – | ✓ | – | – |
| 5 | Rainbow | – | ✓ | ✓ | 1 |
| 6 | Breath_rainbow | – | ✓ | – | – |
| 7 | Comet | ✓ | ✓ | ✓ | 2 |
| 8 | Rainbow_Comet | – | ✓ | ✓ | 2 |
| 9 | Moving_Flash | ✓ | ✓ | ✓ | 2 |
| 10 | Flash_and_Dash | – | ✓ | ✓ | 2 |
| 11 | Wave | – | ✓ | – | – |
| 12 | Glowing_Yoyo | – | ✓ | – | – |
| 13 | Starry_Night | – | ✓ | – | – |
| 0xFF | Default | – | – | – | – |

IDs 2–13 match `src/protocol.rs` `MoboMode::from_code` 2–13 exactly (Breathing…RandomFlicker; our `Chase`=9, `Wave`=11, `RandomFlicker`=13 etc.) — independent confirmation of the mode vocabulary. Differences: the vendor uses **ID 0 = Static** (sent with the direct flag, buffer `0x8100`) and has no `1`; luminactl uses `0 = Off`, `1 = Static`. `ID 1` and `Off` do not exist in this table.

Speed/direction wire format (`FUN_180009170`, header block `{direct, mode, speed, direction}`):

```c
direct    = (effectId == 0);                         // Static -> direct buffer 0x8100, else 0x8160
mode      = effectId == 0xFF ? 5 : effectId;         // 0xFF "Default" is sent as mode 5
speed     = (v < 3 || v == 0xFE || v == 0xFF) ? v : 0;   // 0x8022
direction = (v < 3) ? v : 0;                              // 0x8023
```

* **Speed levels: `0,1,2` (and `0xFE`/`0xFF`) → `0x8022`; direction `0,1,2` → `0x8023`.** Any other value is silently replaced by `0`. Mapping onto "slow/medium/fast" and "forward/reverse/other" is *inferred* from 0-based ordering; the binary names none of them.
* Validation (`FUN_18000a940`): `speed ≤ maxSpeed (0xFF)` and, for direction-capable effects, `speed ≤ maxDirection` (1 or 2). This looks like a vendor bug (speed compared with the direction limit) and effectively caps speed at 1 (Rainbow) or 2 (Comet…Flash_and_Dash).
* `FUN_180009290` converts a caller time value (30 ms ticks, `t/0x1e`) per effect into one byte and sends it through vtbl slot `+0x20` → **`0x80F2 ← byte`** (effect-dependent modulus: id2 `÷188`, ids3/11 `÷50`, ids4/5 byte, 7/8 `%16` (or `%40` when flag==2) with `[3, n-3)` window, 9/10 `%30` (`%60`), 12 `%112` window `[6,106]`, 13 `%25` window `[3,21]`). Meaning of `0x80F2` is **unknown** (inferred: effect period/length).

### 6.4 Compare with `src/mobo.rs`

| Aspect | ENE HAL (ASM USB) | luminactl (`src/mobo.rs`, LED-0116 SMBus) | Verdict |
|--------|--------------------|-------------------------------------------|---------|
| Transport | ASMedia SCSI vendor cmd `0xEC/0x41`, 16-bit index | NCT6793D SMBus, 16-bit index via `0x00/0x01` | ❌ different, no sharing |
| Direct/mode/speed/dir regs | `0x8020…0x8023` | same | ✅ same layout (ENE-family) |
| Apply | `0x802F ← 1` (V2) | `0x80A0 ← 1` (V1) | ⚠️ per-firmware, keep V1 for LED-0116 |
| Colour buffers | `0x8100` / `0x8160` (V2) | `0x8000` / `0x8010` (V1) | ⚠️ per-firmware |
| Colour byte order | R,B,G | R,B,G | ✅ |
| Mode IDs | 0 Static, 2–13, 0xFF Default | 0 Off, 1 Static, 2–13 | ✅ 2–13; ⚠️ 0/1 differ |
| Speed values | `0,1,2,0xFE,0xFF`, else `0` | `u8` written as given | ⚠️ unvalidated in luminactl |
| Direction values | `0,1,2`, else `0` | `u8`; status maps `0=forward`, else `reverse` | ⚠️ `2` exists vendor-side |
| Name / config reads | none | `0x1000..`, `0x1C00..` | ❌ no vendor counterpart |
| Save | none (`0x80F1`/`0xAA` not present) | not implemented | ✅ no conflict |

### 6.5 Recommendations (mobo)

1. **Clamp/validate** speed to `0..=2` (plus `0xFE`/`0xFF` only if exposed deliberately) and direction to `0..=2`; reject others instead of writing raw bytes. Vendor firmware tolerates only those values and the HAL silently zeroes the rest.
2. **Per-mode capability table** from §6.3 (speed ✓/✗, direction ✓/✗): don't offer speed on Static, nor direction on modes 2/3/4/6/11/12/13. luminactl's `uses_color()` gating already follows the colour column.
3. **Do not adopt** `0x802F`, `0x8100/0x8160`, `0x80F2` for LED-0116 without a hardware probe; they belong to the ASMedia/V2 firmware.
4. Document that ENE's own HAL does **not** validate `LED-0116`; luminactl's firmware-name gate stays.

## 7. Phase 2 summary table

| # | Finding | Evidence | Recommendation for luminactl |
|---|---------|----------|------------------------------|
| P1 | `AacVgaHal_x64.dll` calls `LED_DLL_forMB_x64.dll`, not `VGA_Extra_x64.dll`; real chain is HAL → LED_DLL_forMB → (IPC or) LED_DLL_forMB_User → VGA_Extra | Observed (`FUN_180015ec0`, `FUN_180002720`) | Correct any docs that say the HAL dispatches to VGA_Extra directly |
| P2 | `fnSetLEDFrame` writes only reg `0x0C ← frame (u8)`; `fnGetLEDFrame` reads reg **`0x0B`**; no caller supplies a value in the HAL | Observed | Keep 0x0C untouched; semantics unknown. Don't label it "sync" |
| P3 | `fnEffectReset` writes reg `0x0D ← 1` only | Observed | Do not implement without a hardware probe |
| P4 | GPU `SetLEDDeviceEffectSpeed/Direction` act **only** on ENE class `0xCE` (`0x8022`/`0x8023`, then `0x802F←1`); no-op on `0x52` | Observed | **No GPU speed/direction feature for the Strix 1060**; do not add flags that imply otherwise |
| P5 | `GetLEDDeviceSpeedLevel(0x52)=1`, `Generations(0x52)=2` | Observed | Expose "speed: n/a" for the GPU in status/docs |
| P6 | `HAL_DLL_X64` is ENE's ASMedia-USB V2 HAL (`0x8020…0x8023`, `0x802F`, `0x8100/0x8160`), not SMBus/LED-0116 | Observed | Don't cite it as ground truth for LED-0116 registers or `0x80A0`/`0x1000`/`0x1C00` |
| P7 | Effect IDs 2–13 and R,B,G packing independently match `protocol.rs`; vendor ID 0 = Static (no Off, no ID 1) | Observed | Keep luminactl IDs; the `0=Off/1=Static` split is luminactl-specific/firmware-driven |
| P8 | Speed `0,1,2,0xFE,0xFF` and direction `0,1,2` accepted; others → 0; speed also capped by direction limit (quirk) | Observed (clamp), Inferred (semantics) | Validate `--speed 0..=2`, direction `0..=2` in `mobo` CLI/MCP/web |
| P9 | Per-effect speed/direction capability table | Observed | Add table to `MoboMode` (`supports_speed`, `supports_direction`) and gate in UI |
| P10 | Unknown: meaning of `0x0C`/`0x0B`/`0x0D` on GPU, `0x80F2` on ENE ASM, speed→time mapping | Unknown | Hardware probe before any write |

Bounded searches (Phase 2): register immediates searched across `ene_all` decompilation of `HAL_DLL_X64` (672 functions, all decompiled) for `0x8020…0x8023, 0x802F, 0x80A0, 0x80F1, 0x80F2, 0x1000, 0x1C00`; `AacVgaHal_x64.dll` (2476 functions) searched for indirect calls through the function table at `this+0x10..0x90`. `EHD_CTRL_ASMEDIA_DLL_x64` was only inspected by exports/strings (ASMedia `AsmtUSBLib_*` wrapper, no LED logic). `AacVga_UserApp_x64.exe` was checked only for the `fnSetLEDDeviceEffectSpeed` string xref (loader `FUN_140003f00`, same GetProcAddress pattern). Nothing was executed against hardware.
