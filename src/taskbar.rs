//! Botones de la miniatura de la barra de tareas de Windows (anterior, reproducir/pausar,
//! siguiente), como en el Spotify oficial. Usa ITaskbarList3 y un subclass del WndProc de la
//! ventana para recibir los clics (WM_COMMAND con THBN_CLICKED).

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;

use souvlaki::MediaControlEvent;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::CreateBitmap;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList3, SetWindowSubclass, TaskbarList, THBF_ENABLED, THBN_CLICKED, THB_FLAGS, THB_ICON, THB_TOOLTIP,
    THUMBBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO, WM_COMMAND};

use crate::bus::{Msg, UiTx};

const ID_PREV: u32 = 1;
const ID_PLAY: u32 = 2;
const ID_NEXT: u32 = 3;

struct Taskbar {
    list: ITaskbarList3,
    hwnd: HWND,
    play: HICON,
    pause: HICON,
}

thread_local! {
    static TB: RefCell<Option<Taskbar>> = const { RefCell::new(None) };
}
static TX: OnceLock<UiTx> = OnceLock::new();

/// Añade los tres botones a la miniatura de la barra de tareas. Llamar una vez, con la ventana
/// ya visible (el botón de la barra de tareas debe existir).
pub fn install(hwnd: *mut c_void, tx: UiTx) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let _ = TX.set(tx);
    let hwnd = HWND(hwnd);
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let list: ITaskbarList3 = match CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("barra de tareas: {e}");
                return false;
            }
        };
        if let Err(e) = list.HrInit() {
            log::warn!("barra de tareas: {e}");
            return false;
        }
        let prev = icon(Glyph::Prev);
        let play = icon(Glyph::Play);
        let pause = icon(Glyph::Pause);
        let next = icon(Glyph::Next);
        let buttons = [button(ID_PREV, prev, "Anterior"), button(ID_PLAY, play, "Reproducir"), button(ID_NEXT, next, "Siguiente")];
        if let Err(e) = list.ThumbBarAddButtons(hwnd, &buttons) {
            log::warn!("barra de tareas: no se pudieron añadir los botones: {e}");
            return false;
        }
        if !SetWindowSubclass(hwnd, Some(subclass_proc), 1, 0).as_bool() {
            log::warn!("barra de tareas: no se pudo interceptar la ventana");
        }
        TB.with(|tb| *tb.borrow_mut() = Some(Taskbar { list, hwnd, play, pause }));
    }
    true
}

/// Cambia el icono central según el estado (reproducir ↔ pausar).
pub fn set_playing(playing: bool) {
    TB.with(|tb| {
        if let Some(t) = tb.borrow().as_ref() {
            let (icon, tip) = if playing { (t.pause, "Pausar") } else { (t.play, "Reproducir") };
            unsafe {
                let _ = t.list.ThumbBarUpdateButtons(t.hwnd, &[button(ID_PLAY, icon, tip)]);
            }
        }
    });
}

fn button(id: u32, icon: HICON, tip: &str) -> THUMBBUTTON {
    let mut b = THUMBBUTTON {
        dwMask: THB_ICON | THB_TOOLTIP | THB_FLAGS,
        iId: id,
        iBitmap: 0,
        hIcon: icon,
        szTip: [0; 260],
        dwFlags: THBF_ENABLED,
    };
    for (i, u) in tip.encode_utf16().take(259).enumerate() {
        b.szTip[i] = u;
    }
    b
}

unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if msg == WM_COMMAND && ((wparam.0 >> 16) & 0xffff) as u32 == THBN_CLICKED {
        let ev = match (wparam.0 & 0xffff) as u32 {
            ID_PREV => Some(MediaControlEvent::Previous),
            ID_PLAY => Some(MediaControlEvent::Toggle),
            ID_NEXT => Some(MediaControlEvent::Next),
            _ => None,
        };
        if let (Some(ev), Some(tx)) = (ev, TX.get()) {
            tx.send(Msg::Media(ev));
            return LRESULT(0);
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

#[derive(Clone, Copy)]
enum Glyph {
    Prev,
    Play,
    Pause,
    Next,
}

/// Icono blanco de 32×32 dibujado a mano (BGRA premultiplicada).
fn icon(g: Glyph) -> HICON {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    let inside = |x: f32, y: f32| -> bool {
        // Coordenadas 0..1 con el glifo centrado.
        let tri = |ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32| {
            let d1 = (x - bx) * (ay - by) - (ax - bx) * (y - by);
            let d2 = (x - cx) * (by - cy) - (bx - cx) * (y - cy);
            let d3 = (x - ax) * (cy - ay) - (cx - ax) * (y - ay);
            let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            !(neg && pos)
        };
        let rect = |x0: f32, y0: f32, x1: f32, y1: f32| x >= x0 && x <= x1 && y >= y0 && y <= y1;
        match g {
            Glyph::Play => tri(0.28, 0.18, 0.28, 0.82, 0.82, 0.5),
            Glyph::Pause => rect(0.24, 0.2, 0.42, 0.8) || rect(0.58, 0.2, 0.76, 0.8),
            Glyph::Next => tri(0.2, 0.2, 0.2, 0.8, 0.66, 0.5) || rect(0.7, 0.2, 0.82, 0.8),
            Glyph::Prev => tri(0.8, 0.2, 0.8, 0.8, 0.34, 0.5) || rect(0.18, 0.2, 0.3, 0.8),
        }
    };
    for y in 0..N {
        for x in 0..N {
            // Antialiasing sencillo: 4 muestras por píxel.
            let mut hits = 0;
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                if inside((x as f32 + dx) / N as f32, (y as f32 + dy) / N as f32) {
                    hits += 1;
                }
            }
            let a = (hits * 255 / 4) as u8;
            let i = (y * N + x) * 4;
            px[i] = a; // B
            px[i + 1] = a; // G
            px[i + 2] = a; // R
            px[i + 3] = a; // A (premultiplicada: blanco)
        }
    }
    let mask = vec![0u8; N * N / 8];
    unsafe {
        let color = CreateBitmap(N as i32, N as i32, 1, 32, Some(px.as_ptr() as *const c_void));
        let mono = CreateBitmap(N as i32, N as i32, 1, 1, Some(mask.as_ptr() as *const c_void));
        let info = ICONINFO { fIcon: BOOL(1), xHotspot: 0, yHotspot: 0, hbmMask: mono, hbmColor: color };
        CreateIconIndirect(&info).unwrap_or_default()
    }
}
