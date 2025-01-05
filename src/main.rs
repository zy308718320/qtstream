#![allow(dead_code)]

extern crate core;

mod apple;
mod coremedia;
mod qt;
mod qt_device;
mod qt_pkt;
mod qt_value;

use crate::coremedia::sample::{SampleBuffer, MEDIA_TYPE_VIDEO, MEDIA_TYPE_SOUND};
use crate::qt::QuickTime;
use rusty_libimobiledevice::error::IdeviceError;
use rusty_libimobiledevice::idevice;
use std::io::Write;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{mpsc, Arc, Mutex};
use std::{io, thread};
use std::net::{TcpListener, TcpStream};

fn get_apple_device() -> Result<idevice::Device, IdeviceError> {
    let devices = match idevice::get_devices() {
        Ok(d) => d,
        Err(e) => return Err(e),
    };

    for device in devices {
        if device.get_network() {
            continue;
        }

        return Ok(device);
    }

    return Err(IdeviceError::NoDevice);
}

fn handle_client(mut stream: TcpStream, rx: Arc<Mutex<Receiver<Result<SampleBuffer, io::Error>>>>, include_header: bool) {
    loop {
        let message = rx.lock().unwrap().recv().expect("read packet from channel");
        if message.is_err() {
            break;
        }

        let sample_buffer = message.unwrap();

        if sample_buffer.media_type() == MEDIA_TYPE_VIDEO {
            let mut combined_data = Vec::new();

            // Add format description data
            if let Some(fd) = sample_buffer.format_description() {
                combined_data.extend_from_slice(&1u32.to_be_bytes());
                combined_data.extend_from_slice(fd.avc1().sps());
                combined_data.extend_from_slice(&1u32.to_be_bytes());
                combined_data.extend_from_slice(fd.avc1().pps());
            }

            // Add sample data
            if let Some(buf) = sample_buffer.sample_data() {
                let mut cur = buf;
                while cur.len() > 0 {
                    let slice_len = u32::from_be_bytes([cur[0], cur[1], cur[2], cur[3]]) as usize;
                    combined_data.extend_from_slice(&1u32.to_be_bytes());
                    combined_data.extend_from_slice(&cur[4..slice_len + 4]);
                    cur = &cur[slice_len + 4..];
                }
            }

            // Send the combined data
            if !combined_data.is_empty() {
                if include_header {
                    let mut data_to_send = Vec::new();
                    let data_length = combined_data.len() as u32;
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("Time went backwards")
                        .as_millis() as u64;
                    data_to_send.extend_from_slice(&data_length.to_le_bytes());
                    data_to_send.extend_from_slice(&timestamp.to_le_bytes());
                    data_to_send.append(&mut combined_data);
                    stream.write_all(&data_to_send).expect("write combined data with length and timestamp");
                } else {
                    stream.write_all(&combined_data).expect("write combined data");
                }
            }
        }

        if sample_buffer.media_type() == MEDIA_TYPE_SOUND {
            println!("MEDIA_TYPE_SOUND");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut udid = None;
    let mut port = Some(12345u16); // Default port
    let mut include_header = false;

    // Parse command line arguments
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-u" if i + 1 < args.len() => {
                udid = Some(args[i + 1].clone());
                i += 1;
            }
            "-p" if i + 1 < args.len() => {
                port = args[i + 1].parse().ok();
                i += 1;
            }
            "-i" => {
                include_header = true;
            }
            _ => {}
        }
        i += 1;
    }

    let device = match get_apple_device() {
        Ok(d) => d,
        Err(e) => {
            println!("get_apple_device: {:?}", e);
            return;
        }
    };

    let lockdownd = match device.new_lockdownd_client("qtstream") {
        Ok(client) => client,
        Err(e) => {
            println!("new_lockdownd_client: {:?}", e);
            return;
        }
    };

    let sn = if let Some(u) = udid {
        u
    } else {
        match lockdownd.get_device_udid() {
            Ok(sn) => sn,
            Err(e) => {
                println!("get_device_udid: {:?}", e);
                return;
            }
        }
    };

    let usb_device = match apple::get_usb_device(sn.replace("-", "").as_str()) {
        Ok(d) => d,
        Err(e) => {
            println!("libusb: {:?}", e);
            return;
        }
    };

    let (tx, rx): (
        SyncSender<Result<SampleBuffer, io::Error>>,
        Receiver<Result<SampleBuffer, io::Error>>,
    ) = mpsc::sync_channel(256);

    let mut qt = QuickTime::new(usb_device, tx);

    match qt.init() {
        Err(e) => {
            println!("init qt failed {}", e);
            return;
        }
        _ => {}
    }

    let t = thread::spawn(move || {
        match qt.run() {
            Err(e) => {
                println!("quick time loop exit: {}", e)
            }
            _ => {}
        };
    });

    let port = port.unwrap_or(12345);
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).expect("Failed to bind to address");
    println!("Server listening on port {}", port);

    let rx = Arc::new(Mutex::new(rx));

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                println!("New connection: {}", stream.peer_addr().unwrap());
                let rx_clone = Arc::clone(&rx);
                let include_header = include_header;
                thread::spawn(move || {
                    handle_client(stream, rx_clone, include_header);
                });
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
    }

    t.join().expect("loop thread term");
}
