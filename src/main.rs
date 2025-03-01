#![allow(dead_code)]

extern crate core;

mod apple;
mod coremedia;
mod qt;
mod qt_device;
mod qt_pkt;
mod qt_value;
mod tcp_server;

use crate::coremedia::sample::{SampleBuffer, MEDIA_TYPE_SOUND, MEDIA_TYPE_VIDEO};
use crate::qt::QuickTime;
use crate::tcp_server::TcpServer;
use rusty_libimobiledevice::error::IdeviceError;
use rusty_libimobiledevice::idevice;
use std::sync::mpsc;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::sync::atomic::AtomicBool;
use std::{io, thread};

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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut udid = None;
    let mut port = Some(12345u16); // Default port
    let mut include_header = false;
    let mut no_audio = false;

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
            "-na" => {
                no_audio = true;
            }
            _ => {}
        }
        i += 1;
    }

    let sn = if let Some(u) = udid {
        u
    } else {
        println!("No udid specified, trying to find a device");

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

    let video_port = port.unwrap_or(12345);
    let audio_port = video_port + 1;

    let video_addr = format!("0.0.0.0:{}", video_port);
    let audio_addr = format!("0.0.0.0:{}", audio_port);

    let audio_connected = Arc::new(AtomicBool::new(false));
    let audio_connected_clone = audio_connected.clone();

    let (video_tx, video_rx): (
        SyncSender<Result<SampleBuffer, io::Error>>,
        Receiver<Result<SampleBuffer, io::Error>>,
    ) = mpsc::sync_channel(256);

    let (audio_tx, audio_rx): (
        SyncSender<Result<SampleBuffer, io::Error>>,
        Receiver<Result<SampleBuffer, io::Error>>,
    ) = mpsc::sync_channel(256);

    let mut qt = QuickTime::new(usb_device, video_tx, audio_tx, no_audio, audio_connected.clone());

    let video_server = TcpServer::new(video_addr, video_rx, MEDIA_TYPE_VIDEO, Some(include_header), None);
    let audio_server = TcpServer::new(audio_addr, audio_rx, MEDIA_TYPE_SOUND, None, Some(audio_connected_clone));

    match qt.init() {
        Err(e) => {
            println!("init qt failed {}", e);
            return;
        }
        _ => {}
    }

    let vt = thread::spawn(move || {
        video_server.run();
    });

    let at = thread::spawn(move || {
        if !no_audio {
            audio_server.run();
        }
    });

    let qt = Arc::new(Mutex::new(qt));
    let qt_clone = Arc::clone(&qt);

    let qtt = thread::spawn(move || {
        let mut qt = qt.lock().unwrap();
        match qt.run() {
            Err(e) => {
                println!("qt loop exit: {}", e)
            }
            _ => {}
        }
    });

    at.join().expect("audio thread term");

    vt.join().expect("video thread term");

    qtt.join().expect("qt thread term");

    drop(qt_clone.lock().unwrap());
}
