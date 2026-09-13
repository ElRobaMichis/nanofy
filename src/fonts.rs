//! Tipografía: una fuente geométrica del sistema como principal (con familia negrita para
//! títulos) y fuentes de respaldo para alfabetos que no cubre (chino, japonés, coreano,
//! árabe, hebreo, tailandés…).
//!
//! Los archivos se mapean en memoria y no se copian: solo las páginas que se tocan (tabla de
//! caracteres y los glifos usados) cuentan en la RAM del proceso, no los 10–20 MB del archivo.

use std::fs::File;

use egui::epaint::text::{FontData, FontInsert, FontPriority, InsertFontFamily};
use egui::FontFamily;

/// (regular, negrita) candidatos para la fuente principal, por orden de preferencia.
#[cfg(target_os = "windows")]
const PRIMARY: &[(&str, &str)] = &[
    ("C:/Windows/Fonts/segoeui.ttf", "C:/Windows/Fonts/segoeuib.ttf"),
];
#[cfg(target_os = "macos")]
const PRIMARY: &[(&str, &str)] = &[
    ("/System/Library/Fonts/Supplemental/Arial.ttf", "/System/Library/Fonts/Supplemental/Arial Bold.ttf"),
];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const PRIMARY: &[(&str, &str)] = &[
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf"),
    ("/usr/share/fonts/noto/NotoSans-Regular.ttf", "/usr/share/fonts/noto/NotoSans-Bold.ttf"),
    ("/usr/share/fonts/TTF/DejaVuSans.ttf", "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"),
];

#[cfg(target_os = "windows")]
const FALLBACKS: &[&str] = &[
    "C:/Windows/Fonts/YuGothM.ttc",  // japonés (kana + kanji)
    "C:/Windows/Fonts/meiryo.ttc",   // japonés
    "C:/Windows/Fonts/msyh.ttc",     // chino simplificado
    "C:/Windows/Fonts/msjh.ttc",     // chino tradicional
    "C:/Windows/Fonts/malgun.ttf",   // coreano
    "C:/Windows/Fonts/seguiemj.ttf", // emoji
];
#[cfg(target_os = "macos")]
const FALLBACKS: &[&str] = &[
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
];
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const FALLBACKS: &[&str] = &[
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/OTF/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
];

fn map(path: &str) -> Option<&'static [u8]> {
    let file = File::open(path).ok()?;
    // SAFETY: los archivos de fuente del sistema no cambian mientras la app está abierta.
    let mmap = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    // El mapeo vive hasta que termina el proceso (egui necesita `'static`).
    Some(&Box::leak(Box::new(mmap))[..])
}

fn family(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

pub fn install_system_fonts(ctx: &egui::Context) {
    let bold = family("bold");
    let mut primary_ok = false;
    for (reg, bld) in PRIMARY {
        let (Some(r), Some(b)) = (map(reg), map(bld)) else {
            continue;
        };
        ctx.add_font(FontInsert::new(
            "nanofy-regular",
            FontData::from_static(r),
            vec![
                InsertFontFamily {
                    family: FontFamily::Proportional,
                    priority: FontPriority::Highest,
                },
                // La familia negrita también necesita la regular como respaldo.
                InsertFontFamily {
                    family: bold.clone(),
                    priority: FontPriority::Lowest,
                },
            ],
        ));
        ctx.add_font(FontInsert::new(
            "nanofy-bold",
            FontData::from_static(b),
            vec![InsertFontFamily {
                family: bold.clone(),
                priority: FontPriority::Highest,
            }],
        ));
        primary_ok = true;
        break;
    }
    if !primary_ok {
        // Sin fuente negrita del sistema: la familia "bold" usa la fuente por defecto de egui.
        let fonts = egui::FontDefinitions::default();
        if let Some(first) = fonts.families.get(&FontFamily::Proportional).and_then(|v| v.first()) {
            if let Some(data) = fonts.font_data.get(first) {
                ctx.add_font(FontInsert::new(
                    "nanofy-bold",
                    (**data).clone(),
                    vec![InsertFontFamily {
                        family: bold.clone(),
                        priority: FontPriority::Highest,
                    }],
                ));
            }
        }
    }

    let mut added = 0;
    for path in FALLBACKS {
        let Some(bytes) = map(path) else {
            continue;
        };
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        ctx.add_font(FontInsert::new(
            &name,
            FontData::from_static(bytes),
            vec![
                InsertFontFamily {
                    family: FontFamily::Proportional,
                    priority: FontPriority::Lowest,
                },
                InsertFontFamily {
                    family: FontFamily::Monospace,
                    priority: FontPriority::Lowest,
                },
                InsertFontFamily {
                    family: bold.clone(),
                    priority: FontPriority::Lowest,
                },
            ],
        ));
        added += 1;
    }
    log::info!("fuentes del sistema: principal={primary_ok}, respaldo={added}");
}
