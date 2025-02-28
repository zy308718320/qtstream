use crate::coremedia::sample::{SampleBuffer, MEDIA_TYPE_SOUND, MEDIA_TYPE_VIDEO};

use std::io;
use std::io::Write;
use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct TcpServer {
    address: String,
    rx: Receiver<Result<SampleBuffer, io::Error>>,
    media_type: u32,
    include_header: bool,
    connected_state: Option<Arc<AtomicBool>>,
}

impl AsRef<TcpServer> for TcpServer {
    fn as_ref(&self) -> &TcpServer {
        self
    }
}

impl TcpServer {
    pub fn new(
        address: String,
        rx: Receiver<Result<SampleBuffer, io::Error>>,
        media_type: u32,
        include_header: Option<bool>,
        connected_state: Option<Arc<AtomicBool>>,
    ) -> TcpServer {
        return TcpServer {
            address,
            rx,
            media_type,
            include_header: include_header.unwrap_or(false),
            connected_state,
        };
    }

    pub fn run(&self) {
        let media_type_str = match self.media_type {
            MEDIA_TYPE_SOUND => "audio",
            MEDIA_TYPE_VIDEO => "video",
            _ => "unknown",
        };
        let listener = TcpListener::bind(&self.address).expect("Failed to bind address");

        println!(
            "{} server started on port {}",
            media_type_str,
            listener.local_addr().unwrap().port()
        );

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    println!(
                        "New {} connection: {}",
                        media_type_str,
                        stream.peer_addr().unwrap()
                    );

                    self.handle_send(stream);
                }
                Err(e) => {
                    eprintln!("{} connection error: {}", media_type_str, e);
                }
            }
        }
    }

    fn handle_send(&self, mut stream: std::net::TcpStream) {
        if let Some(state) = &self.connected_state {
            state.store(true, Ordering::SeqCst);
        }

        loop {
            let message = match self.rx.try_recv() {
                Ok(msg) => msg,
                Err(mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                Err(_) => break,
            };

            let sample_buffer = message.unwrap();
            let buf = sample_buffer.sample_data().unwrap();

            if self.media_type == MEDIA_TYPE_SOUND {
                stream.write_all(buf).expect("Failed to write audio data");
            } else {
                let mut combined_data = Vec::new();

                // Add format description data
                if let Some(fd) = sample_buffer.format_description() {
                    combined_data.extend_from_slice(&1u32.to_be_bytes());
                    combined_data.extend_from_slice(fd.avc1().sps());
                    combined_data.extend_from_slice(&1u32.to_be_bytes());
                    combined_data.extend_from_slice(fd.avc1().pps());
                }

                // Add sample data
                let mut cur = buf;
                while cur.len() > 0 {
                    let slice_len = u32::from_be_bytes([cur[0], cur[1], cur[2], cur[3]]) as usize;
                    combined_data.extend_from_slice(&1u32.to_be_bytes());
                    combined_data.extend_from_slice(&cur[4..slice_len + 4]);
                    cur = &cur[slice_len + 4..];
                }

                // Send the combined data
                if self.include_header {
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
}
