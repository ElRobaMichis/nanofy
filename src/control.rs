//! Modo de control local para pruebas automatizadas (`--control <puerto>`).
//!
//! Escucha solo en 127.0.0.1. Cada línea recibida es un JSON `{"op": "...", ...}`; se entrega a
//! la interfaz por el bus (como cualquier otro mensaje) y se ejecuta en el hilo de la interfaz
//! por los mismos métodos que usan los botones y atajos. La respuesta es una línea JSON.
//! Ver `src/app/control.rs` para las operaciones y el estado que se devuelve.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

use crate::bus::{Msg, UiTx};

pub struct ControlReq {
    pub cmd: serde_json::Value,
    pub reply: mpsc::Sender<serde_json::Value>,
}

pub fn start(port: u16, ui: UiTx) {
    let spawn = std::thread::Builder::new()
        .name("nanofy-control".to_string())
        .spawn(move || {
            let mut listener = None;
            for _ in 0..50 {
                match TcpListener::bind(("127.0.0.1", port)) {
                    Ok(l) => {
                        listener = Some(l);
                        break;
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
            let Some(listener) = listener else {
                log::error!("[control] no se pudo escuchar en 127.0.0.1:{port}");
                return;
            };
            log::info!("[control] escuchando en 127.0.0.1:{port}");
            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let ui = ui.clone();
                        let _ = std::thread::Builder::new()
                            .name("nanofy-control-conn".to_string())
                            .spawn(move || serve(s, ui));
                    }
                    Err(e) => log::warn!("[control] conexión fallida: {e}"),
                }
            }
        });
    if let Err(e) = spawn {
        log::error!("[control] no se pudo lanzar el hilo: {e}");
    }
}

fn serve(stream: TcpStream, ui: UiTx) {
    let mut out = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(cmd) => {
                let (tx, rx) = mpsc::channel();
                ui.send(Msg::Control(ControlReq { cmd, reply: tx }));
                match rx.recv_timeout(Duration::from_secs(20)) {
                    Ok(v) => v,
                    Err(_) => serde_json::json!({"ok": false, "error": "la interfaz no respondió en 20 s"}),
                }
            }
            Err(e) => serde_json::json!({"ok": false, "error": format!("JSON no válido: {e}")}),
        };
        let mut text = reply.to_string();
        text.push('\n');
        if out.write_all(text.as_bytes()).is_err() {
            break;
        }
    }
}
