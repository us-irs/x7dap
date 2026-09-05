// Copyright 2025 Adam Greig
// Licensed under Apache-2.0 and MIT licenses.

use anyhow::{Context, bail};
use clap::{Arg, ArgAction, crate_description, crate_version, value_parser};
use clap_num::si_number;
use probe_rs::config::ScanChainElement;
use probe_rs::probe::WireProtocol;
use probe_rs::probe::list::Lister;
use std::time::{Duration, Instant};
use x7dap::{
    Bitstream, X7, X7IdCode, auto_tap_idx, check_tap_idx, fixup_zynq_ir_lengths,
    idcode_from_scan_chain_name,
};

/// Default JTAG clock frequency. The actual achievable maximum depends on the probe (e.g. which
/// FTDI chip it uses - see `probe/ftdi/mod.rs`'s per-chip `max_clock`), not on the target device,
/// so this stays a plain conservative default rather than something auto-tuned per detected
/// device - use `--freq` to raise it for a specific probe.
const DEFAULT_FREQ_HZ: &str = "1M";

fn main() -> anyhow::Result<()> {
    let matches = clap::Command::new("x7dap")
        .version(crate_version!())
        .about(crate_description!())
        .subcommand_required(true)
        .arg_required_else_help(true)
        .propagate_version(true)
        .infer_subcommands(true)
        .arg(Arg::new("quiet")
             .help("Suppress informative output and raise log level to errors only")
             .long("quiet")
             .short('q')
             .action(ArgAction::SetTrue)
             .global(true))
        .arg(Arg::new("verbose")
             .help("Increase log level, specify once for info, twice for debug, three times for trace")
             .long("verbose")
             .short('v')
             .action(ArgAction::Count)
             .conflicts_with("quiet")
             .global(true))
        .arg(Arg::new("probe")
             .help("VID:PID[:SN] of the probe-rs debug probe to use")
             .long("probe")
             .short('p')
             .action(ArgAction::Set)
             .global(true))
        .arg(Arg::new("freq")
             .help("JTAG clock frequency in Hz (k and M suffixes allowed)")
             .long("freq")
             .short('f')
             .action(ArgAction::Set)
             .default_value(DEFAULT_FREQ_HZ)
             .value_parser(si_number::<u32>)
             .global(true))
        .arg(Arg::new("tap")
             .help("Device's TAP position in scan chain (0-indexed, see `scan` output)")
             .long("tap")
             .short('t')
             .action(ArgAction::Set)
             .value_parser(value_parser!(usize))
             .global(true))
        .arg(Arg::new("ir-lengths")
             .help("Lengths of each IR, starting from TAP 0, comma-separated")
             .long("ir-lengths")
             .short('i')
             .action(ArgAction::Set)
             .value_delimiter(',')
             .value_parser(value_parser!(usize))
             .global(true))
        .subcommand(clap::Command::new("probes")
            .about("List available debug probes"))
        .subcommand(clap::Command::new("scan")
            .about("Scan JTAG chain and detect 7-series IDCODEs"))
        .subcommand(clap::Command::new("reset")
            .about("Pulse the JTAG nRST line for 100ms"))
        .subcommand(clap::Command::new("reload")
            .about("Request the device reload its configuration"))
        .subcommand(clap::Command::new("dna")
            .about("Read the device DNA"))
        .subcommand(clap::Command::new("status")
            .about("Read the device status register"))
        .subcommand(clap::Command::new("xadc")
            .about("Read the XADC values"))
        .subcommand(clap::Command::new("program")
            .about("Program SRAM with bitstream")
            .arg(Arg::new("file")
                 .help("File to program to device")
                 .required(true)))
        .get_matches();

    let t0 = Instant::now();
    let quiet = matches.get_flag("quiet");
    let verbose = matches.get_count("verbose");
    let env = if quiet {
        env_logger::Env::default().default_filter_or("error")
    } else if verbose == 0 {
        env_logger::Env::default().default_filter_or("warn")
    } else if verbose == 1 {
        env_logger::Env::default().default_filter_or("info")
    } else if verbose == 2 {
        env_logger::Env::default().default_filter_or("debug")
    } else {
        env_logger::Env::default().default_filter_or("trace")
    };
    env_logger::Builder::from_env(env)
        .format_timestamp(None)
        .init();

    // Listing probes does not require first connecting to a probe,
    // so we just list them and quit early.
    if matches.subcommand_name().unwrap_or("") == "probes" {
        print_probe_list();
        return Ok(());
    }

    // All functions after this point require an open probe, so
    // we now attempt to connect to the specified probe.
    let lister = Lister::new();
    let mut probe = if let Some(selector) = matches.get_one::<String>("probe") {
        lister.open(selector.parse::<probe_rs::probe::DebugProbeSelector>()?)?
    } else {
        let probes = lister.list_all();
        let info = probes.first().with_context(|| "No debug probes found")?;
        info.open()?
    };

    probe.select_protocol(WireProtocol::Jtag)?;

    // If the user specified a JTAG clock frequency, apply it now, before
    // attaching: attach_to_unspecified() below does its own internal scan
    // at whatever speed the probe currently has, so this has to happen
    // first for that scan to run at the right speed too.
    if let Some(&freq) = matches.get_one::<u32>("freq") {
        // Round up rather than truncate: 0 kHz has special meaning to some backends (e.g.
        // FTDI treats it as "use the maximum supported speed" instead of a slow one), so a
        // sub-1000Hz --freq truncating to 0 would silently do the opposite of what was asked.
        probe.set_speed(freq.div_ceil(1000))?;
    }

    // Actually initializes the probe (for FTDI: usb_reset, set the MPSSE bit
    // mode, latency timer, purge buffers, drain leftover data, pin config,
    // clock speed, disable loopback). Neither select_protocol nor
    // try_as_jtag_probe do this, so without it every JTAG operation below
    // would run against whatever state the chip happened to already be in.
    probe.attach_to_unspecified()?;

    // At this point we can handle the reset command.
    if matches.subcommand_name().unwrap_or("") == "reset" {
        if !quiet {
            println!("Pulsing nRST line.")
        };
        probe.target_reset_assert()?;
        std::thread::sleep(Duration::from_millis(100));
        probe.target_reset_deassert()?;
        return Ok(());
    }

    let jtag = probe
        .try_as_jtag_probe()
        .with_context(|| "Selected probe does not support JTAG")?;

    // If the user specified IR lengths, provide them as the expected scan
    // chain so the automatic IR length detection can use them to resolve
    // otherwise-ambiguous chains.
    if let Some(ir_lens) = matches.get_many::<usize>("ir-lengths") {
        let expected: Vec<ScanChainElement> = ir_lens
            .map(|&len| ScanChainElement {
                name: None,
                ir_len: Some(len as u8),
            })
            .collect();
        jtag.set_expected_scan_chain(&expected)?;
    }

    // Scan the JTAG chain to detect all available TAPs.
    let chain = jtag.scan_chain()?.to_vec();

    // If the user specified a TAP, we'll use it, but otherwise attempt to find a single FPGA
    // in the scan chain. auto_tap_idx/check_tap_idx only look at each TAP's IDCODE, never at
    // its IR length, so both give a trustworthy answer even before the IR length fixup below
    // - which is why detection happens first here, rather than the other way around. Not
    // fatal if this comes back empty: the 'scan' command still works without a confirmed
    // device, so bailing out is deferred until we know that's actually needed.
    let manual_tap_idx = matches.get_one::<usize>("tap").copied();
    let detected = match manual_tap_idx {
        Some(tap_idx) => check_tap_idx(&chain, tap_idx).map(|idcode| (tap_idx, idcode)),
        None => auto_tap_idx(&chain),
    };

    // Zynq-7000's two-TAP PL/PS chain is a shape the generic IR length detection can get
    // backward without reporting an error (see fixup_zynq_ir_lengths). Force the known-correct
    // split now that a Zynq-7000 is confirmed present.
    let chain = match detected {
        Some((_, idcode)) if idcode.is_zynq7000() => match fixup_zynq_ir_lengths(&chain) {
            Some(fixed) => {
                log::info!(
                    "Detected a Zynq-7000 PL/PS chain with a mis-detected IR length split, \
                     correcting it"
                );
                jtag.set_scan_chain(&fixed)?;
                fixed
            }
            None => chain,
        },
        _ => chain,
    };

    // At this point we can handle the 'scan' command.
    if matches.subcommand_name().unwrap_or("") == "scan" {
        print_jtag_chain(&chain);
        return Ok(());
    }

    let (tap_idx, idcode) = match (detected, manual_tap_idx) {
        (Some(detected), _) => detected,
        (None, Some(tap_idx)) => {
            print_jtag_chain(&chain);
            bail!("The provided tap index {tap_idx} does not have an 7-series IDCODE.");
        }
        (None, None) => {
            print_jtag_chain(&chain);
            bail!("Could not find an 7-series IDCODE in the JTAG chain.");
        }
    };

    jtag.select_target(tap_idx)?;
    let mut x7 = X7::new(jtag, idcode);

    match matches.subcommand_name() {
        Some("dna") => {
            if !quiet {
                println!("Reading DNA...")
            };
            let dna = x7.dna()?;
            println!(
                "DNA: {}",
                dna.iter()
                    .map(|b| format!("{:02X}", b))
                    .collect::<Vec<_>>()
                    .join("")
            );
        }
        Some("status") => {
            if !quiet {
                println!("Reading status...")
            };
            let status = x7.status()?;
            println!("{status:?}");
        }
        Some("xadc") => {
            if !quiet {
                println!("Reading XADC...")
            };
            let xadc = x7.xadc()?;
            println!("{xadc}");
        }
        Some("reload") => {
            if !quiet {
                println!("Reloading configuration...")
            };
            x7.jprogram()?;
        }
        Some("program") => {
            let matches = matches.subcommand_matches("program").unwrap();
            let path = matches.get_one::<String>("file").unwrap();
            let bitstream = Bitstream::from_path(path)?;
            if quiet {
                x7.program(bitstream.data())?;
            } else {
                x7.program_progress(bitstream.data())?;
            }
        }
        _ => unreachable!("clap guarantees one of the subcommands above"),
    }

    let t1 = t0.elapsed();
    if !quiet {
        println!(
            "Finished in {}.{:02}s",
            t1.as_secs(),
            t1.subsec_millis() / 10
        );
    }

    Ok(())
}

fn print_probe_list() {
    let lister = Lister::new();
    let probes = lister.list_all();
    if !probes.is_empty() {
        println!("The following debug probes were found:");
        for (num, link) in probes.iter().enumerate() {
            println!("[{num}]: {link}");
        }
    } else {
        println!("No debug probes were found.");
    }
}

fn print_jtag_chain(chain: &[ScanChainElement]) {
    println!("Detected JTAG chain, closest to TDO first:");
    for (idx, elem) in chain.iter().enumerate() {
        let ir_len = elem.ir_len();
        match &elem.name {
            Some(name) => {
                let x7 = idcode_from_scan_chain_name(name).and_then(X7IdCode::try_from_u32);
                match x7 {
                    Some(x7) => println!(" - {idx}: {name} [IR length: {ir_len}] [{}]", x7.name()),
                    None => println!(" - {idx}: {name} [IR length: {ir_len}]"),
                }
            }
            None => println!(" - {idx}: [Bypass, IR length: {ir_len}]"),
        }
    }
}
