//! Canal único de mensajes hacia la interfaz. Cada envío despierta un repintado.

use crate::api::ApiResult;
use crate::backend::Event;
use crate::control::ControlReq;
use crate::update::{MoveReason, Stage, UpdateResult};

pub enum Msg {
    Backend(Event),
    Api(ApiResult),
    Image {
        key: String,
        image: Option<egui::ColorImage>,
        /// Sin imagen: el fallo fue pasajero (red, plazo agotado, 5xx) y se reintentará.
        retry: bool,
    },
    /// Portada que salió de la vista antes de descargarse y no se descargó. Lleva el mismo
    /// `Wanted` que su hueco para no quitar uno más nuevo con la misma clave.
    ImageDropped {
        key: String,
        wanted: crate::images::Wanted,
    },
    /// Copia en disco de una playlist leída en el hilo del disco (`None`: ilegible o ya no
    /// estaba). `seq` es el número de esa lectura: solo cuenta la última pedida.
    Warmed {
        id: String,
        seq: u64,
        list: Option<Box<crate::app::CachedList>>,
    },
    Media(souvlaki::MediaControlEvent),
    /// Resultado de la comprobación de versiones (`manual` = pedida desde Ajustes). `location`:
    /// la carpeta del ejecutable y por qué la app no puede actualizarse sola desde ella.
    /// `staged`: esa versión ya está preparada junto al ejecutable (no se descarga otra vez).
    Update { result: UpdateResult, manual: bool, location: Option<(std::path::PathBuf, MoveReason)>, staged: Option<std::path::PathBuf> },
    /// Progreso de la preparación de una versión nueva (descarga, autoprueba) y su final. `turn`
    /// es el de `update::stage`: solo cuenta el de la última preparación pedida.
    UpdateStage { turn: u64, stage: Stage },
    /// Fin de la sustitución del ejecutable: `Ok` con la ruta que hay que abrir al cerrar, o el
    /// texto del aviso.
    UpdateApplied(Result<std::path::PathBuf, String>),
    /// Notas de la versión en uso que guardó la anterior al prepararla (ventana recién
    /// actualizada), leídas en el hilo del disco.
    ReleaseNotes(crate::update::UpdateInfo),
    /// Operación del modo de control (`--control`); se responde por su canal.
    Control(ControlReq),
}

#[derive(Clone)]
pub struct UiTx {
    tx: std::sync::mpsc::Sender<Msg>,
    ctx: egui::Context,
}

impl UiTx {
    pub fn new(tx: std::sync::mpsc::Sender<Msg>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, msg: Msg) {
        if self.tx.send(msg).is_ok() {
            self.ctx.request_repaint();
        }
    }

    pub fn status(&self, text: impl Into<String>) {
        self.send(Msg::Backend(Event::Status(text.into())));
    }

    pub fn error(&self, text: impl Into<String>) {
        self.send(Msg::Backend(Event::Error(text.into())));
    }
}
