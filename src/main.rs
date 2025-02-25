#![allow(dead_code)]

extern crate core;

mod apple;
mod coremedia;
mod qt;
mod qt_device;
mod qt_pkt;
mod qt_value;

use crate::coremedia::sample::SampleBuffer;
use crate::qt::QuickTime;
use rusty_libimobiledevice::error::IdeviceError;
use rusty_libimobiledevice::idevice;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{mpsc, Arc, Mutex};
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

fn handle_video_client(
    mut stream: TcpStream,
    rx: Arc<Mutex<Receiver<Result<SampleBuffer, io::Error>>>>,
    include_header: bool,
) {
    let rx = rx.lock().unwrap();

    loop {
        let message = match rx.try_recv() {
            Ok(msg) => msg,
            Err(mpsc::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            Err(_) => break,
        };

        let sample_buffer = message.unwrap();

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
                stream
                    .write_all(&data_to_send)
                    .expect("write combined data with length and timestamp");
            } else {
                stream
                    .write_all(&combined_data)
                    .expect("write combined data");
            }
        }
    }
}

fn handle_audio_client(
    mut stream: TcpStream,
    rx: Arc<Mutex<Receiver<Result<SampleBuffer, io::Error>>>>,
) {
    let rx = rx.lock().unwrap();

    loop {
        let message = match rx.try_recv() {
            Ok(msg) => msg,
            Err(mpsc::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            Err(_) => break,
        };

        let sample_buffer = message.unwrap();

        if let Some(buf) = sample_buffer.sample_data() {
            stream.write_all(buf).expect("Failed to write audio data");
        }
    }
}

fn run_video_server(
    listener: TcpListener,
    video_rx: Arc<Mutex<Receiver<Result<SampleBuffer, io::Error>>>>,
    include_header: bool,
) {
    println!(
        "Video server started on port {}",
        listener.local_addr().unwrap().port()
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                println!("New video connection: {}", stream.peer_addr().unwrap());
                let rx_clone = Arc::clone(&video_rx);
                handle_video_client(stream, rx_clone, include_header);
            }
            Err(e) => {
                eprintln!("Video connection error: {}", e);
            }
        }
    }
}

fn run_audio_server(
    listener: TcpListener,
    audio_rx: Arc<Mutex<Receiver<Result<SampleBuffer, io::Error>>>>,
) {
    println!(
        "Audio server started on port {}",
        listener.local_addr().unwrap().port()
    );

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                println!("New audio connection: {}", stream.peer_addr().unwrap());
                let rx_clone = Arc::clone(&audio_rx);
                handle_audio_client(stream, rx_clone);
            }
            Err(e) => {
                eprintln!("Audio connection error: {}", e);
            }
        }
    }
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

    let (video_tx, video_rx): (
        SyncSender<Result<SampleBuffer, io::Error>>,
        Receiver<Result<SampleBuffer, io::Error>>,
    ) = mpsc::sync_channel(256);

    let (audio_tx, audio_rx): (
        SyncSender<Result<SampleBuffer, io::Error>>,
        Receiver<Result<SampleBuffer, io::Error>>,
    ) = mpsc::sync_channel(256);

    let mut qt = QuickTime::new(usb_device, video_tx, audio_tx, no_audio);

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

    let video_port = port.unwrap_or(12345);
    let audio_port = video_port + 1;

    let video_addr = format!("0.0.0.0:{}", video_port);
    let audio_addr = format!("0.0.0.0:{}", audio_port);

    let video_listener = TcpListener::bind(&video_addr).expect("Failed to bind to video address");
    println!("Video server listening on port {}", video_port);

    let video_rx = Arc::new(Mutex::new(video_rx));
    let video_rx_clone = Arc::clone(&video_rx);

    let video_server = thread::spawn(move || {
        run_video_server(video_listener, video_rx_clone, include_header);
    });

    video_server.join().expect("video server thread term");

    if !no_audio {
        let audio_listener = TcpListener::bind(&audio_addr).expect("Failed to bind to audio address");
        println!("Audio server listening on port {}", audio_port);

        let audio_rx = Arc::new(Mutex::new(audio_rx));
        let audio_rx_clone = Arc::clone(&audio_rx);

        let audio_server = thread::spawn(move || {
            run_audio_server(audio_listener, audio_rx_clone);
        });

        audio_server.join().expect("audio server thread term");
    }

    t.join().expect("quicktime thread term");
}
