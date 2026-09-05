// Copyright 2025 Adam Greig
// Licensed under the Apache-2.0 and MIT licenses.
#![doc = include_str!("../README.md")]

use bitvec::field::BitField;
use bitvec::vec::BitVec;
use indicatif::{ProgressBar, ProgressStyle};
use num_enum::TryFromPrimitive;
use probe_rs::config::ScanChainElement;
use probe_rs::probe::{DebugProbeError, JtagAccess, JtagSequence};
use std::{convert::TryFrom, fmt, fs::File, io::Read, path::Path, time::Duration};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Device status register in incorrect state.")]
    BadStatus,
    #[error("Cannot access flash memory unless the device is the only TAP in the JTAG chain.")]
    NotOnlyTAP,
    #[error(
        "Bitstream file contains an IDCODE 0x{bitstream:08X} incompatible \
         with the detected device IDCODE 0x{jtag:08X}."
    )]
    IncompatibleIdcode { bitstream: u32, jtag: u32 },
    #[error("Could not remove VERIFY_IDCODE because parsing the bitstream failed")]
    RemoveIdcodeNoMetadata,
    #[error("SPI Flash error")]
    SPIFlash(#[from] spi_flash::Error),
    #[error("JTAG probe error")]
    Probe(#[from] DebugProbeError),
    #[error("I/O error")]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// IDCODEs for all X7 device types.
///
/// IDCODEs are the same between C/A/Q part numbers (e.g. XC7Z030, XA7Z030, XQ7Z030).
///
/// Note first byte is the revision which may vary and so is 0 here.
#[derive(Copy, Clone, Debug, Eq, PartialEq, TryFromPrimitive)]
#[repr(u32)]
pub enum X7IdCode {
    X7S6 = 0x03622093,
    X7S15 = 0x03620093,
    X7S25 = 0x037C4093,
    X7S50 = 0x0362F093,
    X7S75 = 0x037C8093,
    X7S100 = 0x037c7093,
    X7A12T = 0x037c3093,
    X7A15T = 0x0362E093,
    X7A25T = 0x037C2093,
    X7A35T = 0x0362D093,
    X7A50T = 0x0362C093,
    X7A75T = 0x03632093,
    X7A100T = 0x03631093,
    X7A200T = 0x03636093,
    X7K70T = 0x03647093,
    X7K160T = 0x0364C093,
    X7K325T = 0x03651093,
    X7K355T = 0x03747093,
    X7K410T = 0x03656093,
    X7K420T = 0x03752093,
    X7K480T = 0x03751093,
    X7V575T = 0x03671093,
    X7VX330T = 0x03667093,
    X7VX415T = 0x03682093,
    X7VX485T = 0x03687093,
    X7VX550T = 0x03692093,
    X7VX690T = 0x03691093,
    X7VX980T = 0x03696093,
    X7VX1140T = 0x036D5093,
    X7VH580T = 0x036D9093,
    X7VH870T = 0x036DB093,
    X7Z007S = 0x03723093,
    X7Z012S = 0x0373c093,
    X7Z014S = 0x03728093,
    X7Z010 = 0x03722093,
    X7Z015 = 0x0373b093,
    X7Z020 = 0x03727093,
    X7Z030 = 0x0372c093,
    X7Z035 = 0x03732093,
    X7Z045 = 0x03731093,
    X7Z100 = 0x03736093,
}

impl X7IdCode {
    pub fn try_from_u32(idcode: u32) -> Option<Self> {
        Self::try_from(idcode & 0x0FFF_FFFF).ok()
    }

    pub fn try_from_name(name: &str) -> Option<Self> {
        match name.to_ascii_uppercase().as_str() {
            "X7S6" => Some(X7IdCode::X7S6),
            "X7S15" => Some(X7IdCode::X7S15),
            "X7S25" => Some(X7IdCode::X7S25),
            "X7S50" => Some(X7IdCode::X7S50),
            "X7S75" => Some(X7IdCode::X7S75),
            "X7S100" => Some(X7IdCode::X7S100),
            "X7A12T" => Some(X7IdCode::X7A12T),
            "X7A15T" => Some(X7IdCode::X7A15T),
            "X7A25T" => Some(X7IdCode::X7A25T),
            "X7A35T" => Some(X7IdCode::X7A35T),
            "X7A50T" => Some(X7IdCode::X7A50T),
            "X7A75T" => Some(X7IdCode::X7A75T),
            "X7A100T" => Some(X7IdCode::X7A100T),
            "X7A200T" => Some(X7IdCode::X7A200T),
            "X7K70T" => Some(X7IdCode::X7K70T),
            "X7K160T" => Some(X7IdCode::X7K160T),
            "X7K325T" => Some(X7IdCode::X7K325T),
            "X7K355T" => Some(X7IdCode::X7K355T),
            "X7K410T" => Some(X7IdCode::X7K410T),
            "X7K420T" => Some(X7IdCode::X7K420T),
            "X7K480T" => Some(X7IdCode::X7K480T),
            "X7V575T" => Some(X7IdCode::X7V575T),
            "X7VX330T" => Some(X7IdCode::X7VX330T),
            "X7VX415T" => Some(X7IdCode::X7VX415T),
            "X7VX485T" => Some(X7IdCode::X7VX485T),
            "X7VX550T" => Some(X7IdCode::X7VX550T),
            "X7VX690T" => Some(X7IdCode::X7VX690T),
            "X7VX980T" => Some(X7IdCode::X7VX980T),
            "X7VX1140T" => Some(X7IdCode::X7VX1140T),
            "X7VH580T" => Some(X7IdCode::X7VH580T),
            "X7VH870T" => Some(X7IdCode::X7VH870T),
            "X7Z007S" => Some(X7IdCode::X7Z007S),
            "X7Z012S" => Some(X7IdCode::X7Z012S),
            "X7Z014S" => Some(X7IdCode::X7Z014S),
            "X7Z010" => Some(X7IdCode::X7Z010),
            "X7Z015" => Some(X7IdCode::X7Z015),
            "X7Z020" => Some(X7IdCode::X7Z020),
            "X7Z030" => Some(X7IdCode::X7Z030),
            "X7Z035" => Some(X7IdCode::X7Z035),
            "X7Z045" => Some(X7IdCode::X7Z045),
            "X7Z100" => Some(X7IdCode::X7Z100),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            X7IdCode::X7S6 => "X7S6",
            X7IdCode::X7S15 => "X7S15",
            X7IdCode::X7S25 => "X7S25",
            X7IdCode::X7S50 => "X7S50",
            X7IdCode::X7S75 => "X7S75",
            X7IdCode::X7S100 => "X7S100",
            X7IdCode::X7A12T => "X7A12T",
            X7IdCode::X7A15T => "X7A15T",
            X7IdCode::X7A25T => "X7A25T",
            X7IdCode::X7A35T => "X7A35T",
            X7IdCode::X7A50T => "X7A50T",
            X7IdCode::X7A75T => "X7A75T",
            X7IdCode::X7A100T => "X7A100T",
            X7IdCode::X7A200T => "X7A200T",
            X7IdCode::X7K70T => "X7K70T",
            X7IdCode::X7K160T => "X7K160T",
            X7IdCode::X7K325T => "X7K325T",
            X7IdCode::X7K355T => "X7K355T",
            X7IdCode::X7K410T => "X7K410T",
            X7IdCode::X7K420T => "X7K420T",
            X7IdCode::X7K480T => "X7K480T",
            X7IdCode::X7V575T => "X7V575T",
            X7IdCode::X7VX330T => "X7VX330T",
            X7IdCode::X7VX415T => "X7VX415T",
            X7IdCode::X7VX485T => "X7VX485T",
            X7IdCode::X7VX550T => "X7VX550T",
            X7IdCode::X7VX690T => "X7VX690T",
            X7IdCode::X7VX980T => "X7VX980T",
            X7IdCode::X7VX1140T => "X7VX1140T",
            X7IdCode::X7VH580T => "X7VH580T",
            X7IdCode::X7VH870T => "X7VH870T",
            X7IdCode::X7Z007S => "X7Z007S",
            X7IdCode::X7Z012S => "X7Z012S",
            X7IdCode::X7Z014S => "X7Z014S",
            X7IdCode::X7Z010 => "X7Z010",
            X7IdCode::X7Z015 => "X7Z015",
            X7IdCode::X7Z020 => "X7Z020",
            X7IdCode::X7Z030 => "X7Z030",
            X7IdCode::X7Z035 => "X7Z035",
            X7IdCode::X7Z045 => "X7Z045",
            X7IdCode::X7Z100 => "X7Z100",
        }
    }

    /// Returns whether the provided IDCODE is considered compatible with
    /// this IDCODE.
    pub fn compatible(&self, other: X7IdCode) -> bool {
        *self == other
    }

    /// Number of configuration bits per frame.
    ///
    /// Returns (pad_bits_before_frame, bits_per_frame, pad_bits_after_frame).
    pub fn config_bits_per_frame(&self) -> (usize, usize, usize) {
        (0, 0, 0)
    }

    pub fn is_zynq7000(&self) -> bool {
        matches!(
            *self,
            X7IdCode::X7Z007S
                | X7IdCode::X7Z012S
                | X7IdCode::X7Z014S
                | X7IdCode::X7Z010
                | X7IdCode::X7Z015
                | X7IdCode::X7Z020
                | X7IdCode::X7Z030
                | X7IdCode::X7Z035
                | X7IdCode::X7Z045
                | X7IdCode::X7Z100
        )
    }
}

/// Recover the raw 32-bit IDCODE from a scan chain element's name.
///
/// `JtagAccess::scan_chain()` only reports IDCODEs as a display string of the
/// form "0x03727093" or "0x03727093 (Xilinx)", not as the underlying u32, so
/// this parses that fixed prefix back out.
pub fn idcode_from_scan_chain_name(name: &str) -> Option<u32> {
    let hex = name.strip_prefix("0x")?.get(..8)?;
    u32::from_str_radix(hex, 16).ok()
}

pub fn check_tap_idx(chain: &[ScanChainElement], index: usize) -> Option<X7IdCode> {
    let name = chain.get(index)?.name.as_deref()?;
    X7IdCode::try_from_u32(idcode_from_scan_chain_name(name)?)
}

/// The PL's IR length on a Zynq-7000 two-TAP chain, per probe-rs's `Zynq7000.yaml`.
const ZYNQ_PL_IR_LEN: u8 = 6;
/// The PS (ARM DAP) IR length on a Zynq-7000 two-TAP chain, per probe-rs's `Zynq7000.yaml`.
const ZYNQ_PS_IR_LEN: u8 = 4;

/// Index of the PL TAP on a Zynq-7000's own two-TAP chain. Fixed by the chip's internal JTAG
/// daisy-chain, not something that varies per board or probe.
const ZYNQ_PL_TAP_IDX: usize = 0;

/// Correct a mis-detected PL/PS IR length split on a Zynq-7000 scan chain.
///
/// Zynq-7000 always exposes two TAPs on one physical chain: a 6-bit PL
/// (the 7-series configuration TAP this crate talks to) and a 4-bit PS
/// (the ARM DAP). `JtagAccess::scan_chain()`'s generic IR length detection
/// finds exactly two candidate boundaries for this shape, so it does not
/// report an ambiguous chain, but it has no way to know which boundary
/// belongs to which TAP and can attribute the two lengths backward.
///
/// Call this only once a Zynq-7000 is actually confirmed present (e.g. via a resolved
/// `X7IdCode::is_zynq7000`) - it assumes rather than checks the chain shape. Returns `None`
/// if `chain` isn't a two-element chain or the lengths are already correct.
pub fn fixup_zynq_ir_lengths(chain: &[ScanChainElement]) -> Option<Vec<ScanChainElement>> {
    if chain.len() != 2 {
        return None;
    }
    let ps_idx = 1 - ZYNQ_PL_TAP_IDX;
    if chain[ZYNQ_PL_TAP_IDX].ir_len() == ZYNQ_PL_IR_LEN && chain[ps_idx].ir_len() == ZYNQ_PS_IR_LEN
    {
        return None;
    }
    let mut fixed = chain.to_vec();
    fixed[ZYNQ_PL_TAP_IDX].ir_len = Some(ZYNQ_PL_IR_LEN);
    fixed[ps_idx].ir_len = Some(ZYNQ_PS_IR_LEN);
    Some(fixed)
}

/// Attempt to discover a unique TAP index for a 7-series device in a scan chain.
///
/// This only looks at each TAP's IDCODE (`elem.name`), never at `elem.ir_len`,
/// so it is unaffected by IR length mis-detection: it either finds the TAP or
/// it doesn't, regardless of whether the recorded IR length for that TAP is
/// correct (see `fixup_zynq_ir_lengths` for the IR length problem itself).
pub fn auto_tap_idx(chain: &[ScanChainElement]) -> Option<(usize, X7IdCode)> {
    let x7_idxs: Vec<(usize, X7IdCode)> = chain
        .iter()
        .enumerate()
        .filter_map(|(idx, elem)| {
            let Some(name) = elem.name.as_deref() else {
                log::trace!("TAP {idx}: in BYPASS (no IDCODE), skipping");
                return None;
            };
            let Some(idcode) = idcode_from_scan_chain_name(name) else {
                log::trace!("TAP {idx}: could not parse an IDCODE out of {name:?}, skipping");
                return None;
            };
            match X7IdCode::try_from_u32(idcode) {
                Some(id) => {
                    log::debug!("TAP {idx}: IDCODE 0x{idcode:08X} matches {}", id.name());
                    Some((idx, id))
                }
                None => {
                    log::trace!("TAP {idx}: IDCODE 0x{idcode:08X} is not a known 7-series part");
                    None
                }
            }
        })
        .collect();
    let len = x7_idxs.len();
    if len == 0 {
        log::info!("No 7-series device found in JTAG chain");
        None
    } else if len > 1 {
        let indices: Vec<usize> = x7_idxs.iter().map(|(idx, _)| *idx).collect();
        log::info!(
            "Multiple 7-series devices found in JTAG chain at TAPs {indices:?}, specify one using --tap"
        );
        None
    } else {
        let (index, idcode) = x7_idxs.first().unwrap();
        log::debug!(
            "Automatically selecting device at TAP {index} ({})",
            idcode.name()
        );
        Some((*index, *idcode))
    }
}

/// 7-series JTAG instructions.
#[derive(Copy, Clone, Debug)]
#[allow(unused)]
#[repr(u8)]
pub enum Command {
    Extest = 0b100110,
    ExtestPulse = 0b111100,
    ExtestTrain = 0b1111101,
    Sample = 0b000001,
    User1 = 0b000010,
    User2 = 0b000011,
    User3 = 0b100010,
    User4 = 0b100011,
    CfgOut = 0b000100,
    CfgIn = 0b000101,
    UserCode = 0b001000,
    IdCode = 0b001001,
    HighZIo = 0b001010,
    JProgram = 0b001011,
    JStart = 0b001100,
    JShutdown = 0b001101,
    XadcDrp = 0b110111,
    IscEnable = 0b010000,
    IscProgram = 0b010001,
    XscProgramKey = 0b010010,
    XscDna = 0b010111,
    FuseDna = 0b110010,
    IscNoop = 0b010100,
    IscDisable = 0b010110,
    Bypass = 0b111111,
}

#[derive(Copy, Clone, Debug)]
#[allow(unused, non_camel_case_types, clippy::upper_case_acronyms)]
#[repr(u16)]
enum XadcReg {
    Temperature = 0x00,
    Vccint = 0x01,
    Vccaux = 0x02,
    VpVn = 0x03,
    Vrefp = 0x04,
    Vrefn = 0x05,
    Vccbram = 0x06,
    SupplyAOffset = 0x08,
    AdcAOffset = 0x09,
    AdcAGain = 0x0a,
    Vccpint = 0x0d,
    Vccpaux = 0x0e,
    Vccoddr = 0x0f,
    Vaux0 = 0x10,
    Vaux1 = 0x11,
    Vaux2 = 0x12,
    Vaux3 = 0x13,
    Vaux4 = 0x14,
    Vaux5 = 0x15,
    Vaux6 = 0x16,
    Vaux7 = 0x17,
    Vaux8 = 0x18,
    Vaux9 = 0x19,
    Vaux10 = 0x1a,
    Vaux11 = 0x1b,
    Vaux12 = 0x1c,
    Vaux13 = 0x1d,
    Vaux14 = 0x1e,
    Vaux15 = 0x1f,
    MaxTemp = 0x20,
    MaxVccint = 0x21,
    MaxVccaux = 0x22,
    MaxVccbram = 0x23,
    MinTemp = 0x24,
    MinVccint = 0x25,
    MinVccaux = 0x26,
    MinVccbram = 0x27,
    MaxVccpint = 0x28,
    MaxVccpaux = 0x29,
    MaxVccoddr = 0x2a,
    MinVccpint = 0x2c,
    MinVccpaux = 0x2d,
    MinVccoddr = 0x2e,
    SupplyBOffset = 0x30,
    AdcBOffset = 0x31,
    AdcBGain = 0x32,
    Flag = 0x3f,
}

#[derive(Copy, Clone, Debug)]
pub struct MinMaxNow {
    min: f32,
    max: f32,
    current: f32,
    units: &'static str,
}

impl MinMaxNow {
    pub fn from_temperature(min: u16, max: u16, current: u16) -> Self {
        Self {
            min: ((min >> 4) as f32 * 503.975) / 4096.0 - 273.15,
            max: ((max >> 4) as f32 * 503.975) / 4096.0 - 273.15,
            current: ((current >> 4) as f32 * 503.975) / 4096.0 - 273.15,
            units: "°C",
        }
    }

    pub fn from_voltage(min: u16, max: u16, current: u16) -> Self {
        Self {
            min: ((min >> 4) as f32 * 3.0) / 4096.0,
            max: ((max >> 4) as f32 * 3.0) / 4096.0,
            current: ((current >> 4) as f32 * 3.0) / 4096.0,
            units: "V",
        }
    }
}

impl fmt::Display for MinMaxNow {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Min {:.2}{}, Max {:.2}{}, Now {:.2}{}",
            self.min, self.units, self.max, self.units, self.current, self.units,
        )
    }
}

#[derive(Copy, Clone, Debug)]
pub struct XadcReading {
    temperature: MinMaxNow,
    vccint: MinMaxNow,
    vccaux: MinMaxNow,
    vccbram: MinMaxNow,
    vccpint: MinMaxNow,
    vccpaux: MinMaxNow,
    vccoddr: MinMaxNow,
    vrefp: f32,
    vrefn: f32,
    flag: u16,
    is_zynq7000: bool,
}

impl fmt::Display for XadcReading {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.is_zynq7000 {
            write!(
                f,
                " Temperature: {}\n Vccint:  {}\n Vccaux:  {}\n Vccbram: {}\n Vccpint: {}\n \
                  Vccpaux: {}\n Vccoddr: {}\n Vrefp: {:.3}V\n Vrefn: {:.3}V\n Flag: 0x{:04X}",
                self.temperature,
                self.vccint,
                self.vccaux,
                self.vccbram,
                self.vccpint,
                self.vccpaux,
                self.vccoddr,
                self.vrefp,
                self.vrefn,
                self.flag,
            )
        } else {
            write!(
                f,
                " Temperature: {}\n Vccint:  {}\n Vccaux:  {}\n Vccbram: {}\n \
                  Vrefp: {:.3}V\n Vrefn: {:.3}V\n Flag: 0x{:04X}",
                self.temperature,
                self.vccint,
                self.vccaux,
                self.vccbram,
                self.vrefp,
                self.vrefn,
                self.flag,
            )
        }
    }
}

fn vrefp_to_float(vrefp: u16) -> f32 {
    ((vrefp >> 4) as f32) * 3.0 / 4096.0
}

fn vrefn_to_float(vrefn: u16) -> f32 {
    let vrefn = ((vrefn as i16) >> 4) as f32;
    vrefn * 3.0 / 4096.0
}

/// Configuration status register.
#[derive(Copy, Clone)]
pub struct Status(u32);

impl Status {
    pub fn new(word: u32) -> Self {
        Self(word)
    }

    pub fn startup_state(&self) -> u8 {
        ((self.0 >> 18) & 0b111) as u8
    }
    pub fn xadc_overtemp(&self) -> bool {
        self.bit(17)
    }
    pub fn dec_error(&self) -> bool {
        self.bit(16)
    }
    pub fn id_error(&self) -> bool {
        self.bit(15)
    }
    pub fn done(&self) -> bool {
        self.bit(14)
    }
    pub fn release_done(&self) -> bool {
        self.bit(13)
    }
    pub fn init_b(&self) -> bool {
        self.bit(12)
    }
    pub fn init_complete(&self) -> bool {
        self.bit(11)
    }
    pub fn mode(&self) -> u8 {
        ((self.0 >> 8) & 0b111) as u8
    }
    pub fn ghigh_b(&self) -> bool {
        self.bit(7)
    }
    pub fn gwe(&self) -> bool {
        self.bit(6)
    }
    pub fn gts_cfg_b(&self) -> bool {
        self.bit(5)
    }
    pub fn eos(&self) -> bool {
        self.bit(4)
    }
    pub fn dci_match(&self) -> bool {
        self.bit(3)
    }
    pub fn mmcm_lock(&self) -> bool {
        self.bit(2)
    }
    pub fn part_secured(&self) -> bool {
        self.bit(1)
    }
    pub fn crc_error(&self) -> bool {
        self.bit(0)
    }

    fn bit(&self, offset: usize) -> bool {
        (self.0 >> offset) & 1 == 1
    }
}

impl fmt::Debug for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_fmt(format_args!(
            "Status: {:08X}
  Startup state: 0b{:03b}
  XADC overtemp: {}
  Decrypt error: {}
  ID error: {}
  DONE: {}
  Release DONE: {}
  INIT_B: {}
  INIT complete: {}
  Mode: 0b{:03b}
  GHIGH_B: {}
  Global write enable: {}
  Global tri-state: {}
  End of startup: {}
  DCI match: {}
  MMCM lock: {}
  Secured: {}
  CRC error: {}",
            self.0,
            self.startup_state(),
            self.xadc_overtemp(),
            self.dec_error(),
            self.id_error(),
            self.done(),
            self.release_done(),
            self.init_b(),
            self.init_complete(),
            self.mode(),
            self.ghigh_b(),
            self.gwe(),
            self.gts_cfg_b(),
            self.eos(),
            self.dci_match(),
            self.mmcm_lock(),
            self.part_secured(),
            self.crc_error()
        ))
    }
}

pub struct X7<'a> {
    tap: &'a mut dyn JtagAccess,
    idcode: X7IdCode,
}

impl<'a> X7<'a> {
    pub fn new(tap: &'a mut dyn JtagAccess, idcode: X7IdCode) -> Self {
        X7 { tap, idcode }
    }

    pub fn idcode(&self) -> X7IdCode {
        self.idcode
    }

    /// Remain in Run-Test/Idle for `n` TCK cycles.
    ///
    /// `JtagAccess` has no idle-only primitive that is safe to use mid
    /// register transaction (an XADC_DRP read relies on the addressed DR
    /// being left alone between the command and the result shift), so this
    /// drives TMS low directly instead of going through a DR/IR access.
    fn idle(&mut self, n: usize) -> Result<()> {
        self.tap.shift_raw_sequence(JtagSequence {
            tdo_capture: false,
            tms: false,
            data: BitVec::repeat(false, n),
        })?;
        Ok(())
    }

    /// Read full 64-bit device DNA.
    pub fn dna(&mut self) -> Result<Vec<u8>> {
        let dna = self.tap.read_register(Command::FuseDna as u32, 64)?;
        let dna = dna.load_le::<u64>().to_le_bytes().to_vec();
        log::info!("Read DNA: {:02X?}", dna);
        Ok(dna)
    }

    /// Read STATUS register content.
    pub fn status(&mut self) -> Result<Status> {
        self.tap.tap_reset()?;
        self.idle(5)?;

        let mut cfg = Vec::new();
        for word in [
            0xaa99_5566u32,
            0x2000_0000,
            0x2800_e001,
            0x2000_0000,
            0x2000_0000,
        ] {
            cfg.extend_from_slice(&word.reverse_bits().to_le_bytes());
        }
        self.tap
            .write_register(Command::CfgIn as u32, &cfg, cfg.len() as u32 * 8)?;

        let status = self.tap.read_register(Command::CfgOut as u32, 32)?;
        let status = Status::new(status.load_le::<u32>().reverse_bits());
        log::debug!("{:?}", status);
        self.tap.tap_reset()?;
        Ok(status)
    }

    /// Read XADC registers
    pub fn xadc(&mut self) -> Result<XadcReading> {
        // Select XADC mode. The DR content of this first shift is discarded,
        // same as the leading shift of every read_xadc_reg call below.
        self.tap.tap_reset()?;
        self.idle(5)?;
        self.tap
            .write_register(Command::XadcDrp as u32, &[0; 4], 32)?;

        let reading = XadcReading {
            temperature: MinMaxNow::from_temperature(
                self.read_xadc_reg(XadcReg::MinTemp)?,
                self.read_xadc_reg(XadcReg::MaxTemp)?,
                self.read_xadc_reg(XadcReg::Temperature)?,
            ),
            vccint: MinMaxNow::from_voltage(
                self.read_xadc_reg(XadcReg::MinVccint)?,
                self.read_xadc_reg(XadcReg::MaxVccint)?,
                self.read_xadc_reg(XadcReg::Vccint)?,
            ),
            vccaux: MinMaxNow::from_voltage(
                self.read_xadc_reg(XadcReg::MinVccaux)?,
                self.read_xadc_reg(XadcReg::MaxVccaux)?,
                self.read_xadc_reg(XadcReg::Vccaux)?,
            ),
            vccbram: MinMaxNow::from_voltage(
                self.read_xadc_reg(XadcReg::MinVccbram)?,
                self.read_xadc_reg(XadcReg::MaxVccbram)?,
                self.read_xadc_reg(XadcReg::Vccbram)?,
            ),
            vccpint: MinMaxNow::from_voltage(
                self.read_xadc_reg(XadcReg::MinVccpint)?,
                self.read_xadc_reg(XadcReg::MaxVccpint)?,
                self.read_xadc_reg(XadcReg::Vccpint)?,
            ),
            vccpaux: MinMaxNow::from_voltage(
                self.read_xadc_reg(XadcReg::MinVccpaux)?,
                self.read_xadc_reg(XadcReg::MaxVccpaux)?,
                self.read_xadc_reg(XadcReg::Vccpaux)?,
            ),
            vccoddr: MinMaxNow::from_voltage(
                self.read_xadc_reg(XadcReg::MinVccoddr)?,
                self.read_xadc_reg(XadcReg::MaxVccoddr)?,
                self.read_xadc_reg(XadcReg::Vccoddr)?,
            ),
            vrefp: vrefp_to_float(self.read_xadc_reg(XadcReg::Vrefp)?),
            vrefn: vrefn_to_float(self.read_xadc_reg(XadcReg::Vrefn)?),
            flag: self.read_xadc_reg(XadcReg::Flag)?,
            is_zynq7000: self.idcode.is_zynq7000(),
        };

        self.tap.tap_reset()?;
        Ok(reading)
    }

    /// Read single XADC register
    fn read_xadc_reg(&mut self, reg: XadcReg) -> Result<u16> {
        log::debug!("Reading XADC register {:?} ({:04X})", reg, reg as u16);
        let word = 0x0400_0000 | ((reg as u32) << 16);
        self.tap.write_dr(&word.to_le_bytes(), 32)?;
        self.idle(15)?;
        let result = self.tap.write_dr(&[0; 4], 32)?;
        let result = result.load_le::<u32>();
        log::debug!("Got result {:08X}", result);
        Ok(result as u16)
    }

    /// Program a bitstream to SRAM.
    ///
    /// The FPGA is reset and begins running the new bitstream after programming.
    pub fn program(&mut self, data: &[u8]) -> Result<()> {
        self.program_with_callback(data, |_| {})
    }

    /// Program a bitstream to SRAM, with a progress bar.
    ///
    /// The FPGA is reset and begins running the new bitstream after programming.
    pub fn program_progress(&mut self, data: &[u8]) -> Result<()> {
        const DATA_PROGRESS_TPL: &str =
            " {msg} [{bar:40.cyan/black}] {bytes}/{total_bytes} ({bytes_per_sec}; {eta_precise})";
        const DATA_FINISHED_TPL: &str =
            " {msg} [{bar:40.green/black}] {bytes}/{total_bytes} ({bytes_per_sec}; {eta_precise})";
        const DATA_PROGRESS_CHARS: &str = "━╸━";
        let pb = ProgressBar::new(data.len() as u64).with_style(
            ProgressStyle::with_template(DATA_PROGRESS_TPL)
                .unwrap()
                .progress_chars(DATA_PROGRESS_CHARS),
        );
        pb.set_message("Programming");
        pb.set_position(0);

        self.program_with_callback(data, |n| pb.set_position(n as u64))?;

        pb.set_style(
            ProgressStyle::with_template(DATA_FINISHED_TPL)
                .unwrap()
                .progress_chars(DATA_PROGRESS_CHARS),
        );

        pb.finish();
        Ok(())
    }

    /// Program a bitstream to SRAM, calling `cb` with the number of bytes programmed so far.
    ///
    /// The FPGA is reset and begins running the new bitstream after programming.
    /// See UG470 p.166 Table 10-4 for more information.
    pub fn program_with_callback<F: Fn(usize)>(&mut self, data: &[u8], cb: F) -> Result<()> {
        self.check_ready_to_program()?;
        self.tap.tap_reset()?;
        self.tap.write_register(Command::JProgram as u32, &[], 0)?;
        self.tap.tap_reset()?;
        std::thread::sleep(Duration::from_millis(20));

        // Xilinx bitstream bytes are MSb first, but a JTAG DR shift is LSb
        // first per byte, so each byte's bit order needs reversing before
        // it goes on the wire.
        let data: Vec<u8> = data.iter().map(|x| x.reverse_bits()).collect();

        // Select CFG_IN (IR only, no DR touch), then hold Shift-DR open
        // continuously across the entire bitstream via write_dr_partial,
        // chunked only for the FTDI command buffer's sake (see idle()'s
        // comment), exiting only once at the very end. Xilinx's config
        // engine needs one continuous DR shift for the whole bitstream:
        // exiting to Update-DR between chunks leaves the device sitting
        // unconfigured with no error flags at all, as if the data never
        // reached the frame parser past a chunk boundary. Matches
        // openFPGALoader's own CFG_IN loading, which holds Shift-DR open
        // the same way.
        self.tap.write_register(Command::CfgIn as u32, &[], 0)?;

        const CHUNK_BYTES: usize = 8192;
        let mut chunks = data.chunks(CHUNK_BYTES).peekable();
        let mut written = 0;
        while let Some(chunk) = chunks.next() {
            let last = chunks.peek().is_none();
            self.tap
                .write_dr_partial(chunk, chunk.len() as u32 * 8, written == 0, last)?;
            written += chunk.len();
            cb(written);
        }

        // Return to Run-Test/Idle to complete programming.
        self.idle(1)?;

        // Begin startup sequence.
        self.tap.write_register(Command::JStart as u32, &[], 0)?;
        self.idle(2000)?;
        self.tap.tap_reset()?;

        // Check programming was OK.
        self.check_programmed_ok()?;
        self.tap.tap_reset()?;

        Ok(())
    }

    pub fn jprogram(&mut self) -> Result<()> {
        self.tap.write_register(Command::JProgram as u32, &[], 0)?;
        self.idle(2000)?;
        self.tap.tap_reset()?;
        Ok(())
    }

    fn check_ready_to_program(&mut self) -> Result<()> {
        log::debug!("Checking status before programming...");
        let status = self.status()?;
        if !status.init_complete() {
            log::error!("FPGA init not complete");
            return Err(Error::BadStatus);
        }
        if !status.init_b() {
            log::error!("FPGA INIT_B still low");
            return Err(Error::BadStatus);
        }
        Ok(())
    }

    fn check_programmed_ok(&mut self) -> Result<()> {
        log::debug!("Checking status after programming...");
        let status = self.status()?;
        if !status.init_complete() {
            log::error!("Init not complete");
            return Err(Error::BadStatus);
        }
        if !status.init_b() {
            log::error!("INIT_B still low");
            return Err(Error::BadStatus);
        }
        if !status.done() {
            log::error!("DONE still low");
            return Err(Error::BadStatus);
        }
        if !status.release_done() {
            log::error!("DONE not released");
            return Err(Error::BadStatus);
        }
        if status.dec_error() {
            log::error!("Decrypt error");
            return Err(Error::BadStatus);
        }
        if status.id_error() {
            log::error!("ID error");
            return Err(Error::BadStatus);
        }
        if status.crc_error() {
            log::error!("CRC error");
            return Err(Error::BadStatus);
        }
        Ok(())
    }
}

pub struct Bitstream {
    data: Vec<u8>,
}

impl Bitstream {
    /// Open a bitstream from the provided path.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let mut file = File::open(path)?;
        Self::from_file(&mut file)
    }

    /// Open a bitstream from the provided open `File`.
    pub fn from_file(file: &mut File) -> Result<Self> {
        let mut data = if let Ok(metadata) = file.metadata() {
            Vec::with_capacity(metadata.len() as usize)
        } else {
            Vec::new()
        };
        file.read_to_end(&mut data)?;
        Ok(Self::new(data))
    }

    /// Load a bitstream from the provided raw bitstream data.
    pub fn from_data(data: &[u8]) -> Self {
        Self::new(data.to_owned())
    }

    /// Load a bitstream directly from a `Vec<u8>`.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Get the underlying bitstream data.
    pub fn data(&self) -> &[u8] {
        &self.data[..]
    }
}
