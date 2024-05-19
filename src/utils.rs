use colored::Colorize;
use goblin::elf::program_header::PT_LOAD;
use retry::{delay::Fixed, retry};
use rusb::{open_device_with_vid_pid, GlobalContext};

use std::path::PathBuf;
use std::{fs::File, io::Read};

use crate::Opt;

#[derive(Debug)]
pub enum UtilError {
    Elf(goblin::error::Error),
    Dfu(dfu_libusb::Error),
    File(std::io::Error),
}

/// Returns a contiguous bin with 0s between non-contiguous sections and starting address from an elf.
pub fn elf_to_bin(path: PathBuf) -> Result<(Vec<u8>, u32), UtilError> {
    let mut file = File::open(path).map_err(UtilError::File)?;
    let mut buffer = vec![];
    file.read_to_end(&mut buffer).map_err(UtilError::File)?;

    let binary = goblin::elf::Elf::parse(buffer.as_slice()).map_err(UtilError::Elf)?;

    let mut start_address: u64 = 0;
    let mut last_address: u64 = 0;

    let mut data = vec![];
    for (i, ph) in binary
        .program_headers
        .iter()
        .filter(|ph| {
            ph.p_type == PT_LOAD
                && ph.p_filesz > 0
                && ph.p_offset >= u64::from(binary.header.e_ehsize)
                && ph.is_read()
        })
        .enumerate()
    {
        // first time through grab the starting physical address
        if i == 0 {
            start_address = ph.p_paddr;
        }
        // on subsequent passes, if there's a gap between this section and the
        // previous one, fill it with zeros
        else {
            let difference = (ph.p_paddr - last_address) as usize;
            data.resize(data.len() + difference, 0x0);
        }

        data.extend_from_slice(&buffer[ph.p_offset as usize..][..ph.p_filesz as usize]);

        last_address = ph.p_paddr + ph.p_filesz;
    }

    Ok((
        data,
        u32::try_from(start_address)
            .map_err(|e| UtilError::Elf(goblin::error::Error::Malformed(e.to_string())))?,
    ))
}

pub fn flash_bin(binary: &[u8], d: &rusb::Device<GlobalContext>) -> Result<(), UtilError> {
    let mut dfu = dfu_libusb::DfuLibusb::open(
        &rusb::Context::new().unwrap(),
        d.device_descriptor().unwrap().vendor_id(),
        d.device_descriptor().unwrap().product_id(),
        0,
        0,
    )
    .map_err(UtilError::Dfu)?;

    dfu.download_from_slice(binary).map_err(UtilError::Dfu)?;
    Ok(())
}

pub fn vendor_map() -> std::collections::HashMap<String, Vec<(u16, u16)>> {
    maplit::hashmap! {
        "stm32".to_string() => vec![(0x0483, 0xdf11)],
        "gd32vf103".to_string() =>  vec![(0x28e9, 0x0189)],
    }
}

pub fn find_device(opt: &Opt) -> Option<rusb::DeviceHandle<GlobalContext>> {
    let retries = opt.retries;
    let delay = opt.delay;

    let result = retry(Fixed::from_millis(delay as u64).take(retries), || {
        let default_error = Err("no device found");
        if let (Some(v), Some(p)) = (opt.vid, opt.pid) {
            open_device_with_vid_pid(v, p).ok_or("no device found")
        } else if let Some(c) = &opt.chip {
            println!("    {} for a connected {}.", "Searching".green().bold(), c);

            let mut device: Result<rusb::DeviceHandle<GlobalContext>, &'static str> = default_error;

            let vendor = vendor_map();

            if let Some(products) = vendor.get(c) {
                for (v, p) in products {
                    if let Some(d) = open_device_with_vid_pid(*v, *p) {
                        device = Ok(d);
                        break;
                    }
                }
            }

            device
        } else {
            println!(
                "    {} for a connected device with known vid/pid pair.",
                "Searching".green().bold(),
            );

            let devices: Vec<_> = rusb::devices()
                .expect("Error with Libusb")
                .iter()
                .map(|d| d.device_descriptor().unwrap())
                .collect();

            let mut device: Result<rusb::DeviceHandle<GlobalContext>, &'static str> = default_error;

            for d in devices {
                for vendor in vendor_map() {
                    if vendor.1.contains(&(d.vendor_id(), d.product_id())) {
                        if let Some(d) = open_device_with_vid_pid(d.vendor_id(), d.product_id()) {
                            device = Ok(d);
                            break;
                        }
                    }
                }
            }

            device
        }
    });
    result.ok()
}
