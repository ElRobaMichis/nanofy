//! Modelos de datos de la Web API de Spotify (deserialización tolerante) y tipos de la interfaz.
#![allow(dead_code)]

use serde::{Deserialize, Deserializer, Serialize};

/// Un número que debería ser positivo y Spotify a veces manda negativo o con decimales (el
/// `progress_ms` de `/me/player` justo al empezar una canción llega como `-86`): se ajusta a
/// 0..=u32::MAX en vez de perder toda la respuesta.
fn lenient_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let v: Option<f64> = Option::deserialize(d)?;
    Ok(v.filter(|x| x.is_finite()).map(|x| x.clamp(0.0, u32::MAX as f64) as u32))
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Image {
    pub url: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

/// Elige la imagen más pequeña cuyo ancho sea >= `want`; si ninguna cumple, la mayor.
pub fn pick_image(images: &[Image], want: u32) -> Option<&str> {
    let mut best: Option<&Image> = None;
    for im in images {
        let w = im.width.unwrap_or(0);
        match best {
            None => best = Some(im),
            Some(b) => {
                let bw = b.width.unwrap_or(0);
                let im_ok = w >= want;
                let b_ok = bw >= want;
                if (im_ok && (!b_ok || w < bw)) || (!im_ok && !b_ok && w > bw) {
                    best = Some(im);
                }
            }
        }
    }
    best.map(|i| i.url.as_str())
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ArtistRef {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AlbumRef {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub total_tracks: Option<u32>,
    #[serde(default)]
    pub album_type: Option<String>,
}

impl AlbumRef {
    pub fn year(&self) -> &str {
        year_of(self.release_date.as_deref())
    }
    pub fn artists_str(&self) -> String {
        join_artists(&self.artists)
    }
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }
}

fn year_of(date: Option<&str>) -> &str {
    match date {
        Some(d) => {
            let end = d.char_indices().nth(4).map(|(i, _)| i).unwrap_or(d.len());
            &d[..end]
        }
        None => "",
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Track {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub duration_ms: u32,
    #[serde(default)]
    pub explicit: bool,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    #[serde(default)]
    pub album: Option<AlbumRef>,
    #[serde(default)]
    pub is_local: bool,
    #[serde(default)]
    pub track_number: Option<u32>,
    #[serde(default)]
    pub is_playable: Option<bool>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Usuario que añadió la canción a la playlist (solo en playlists, por librespot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
    /// Fecha (AAAA-MM-DD) en que se añadió a la playlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_at: Option<String>,
}

impl Track {
    pub fn artists_str(&self) -> String {
        join_artists(&self.artists)
    }
    pub fn cover(&self, want: u32) -> Option<&str> {
        self.album.as_ref().and_then(|a| pick_image(&a.images, want))
    }
    pub fn album_name(&self) -> &str {
        self.album.as_ref().map(|a| a.name.as_str()).unwrap_or("")
    }
}

pub fn join_artists(artists: &[ArtistRef]) -> String {
    let mut s = String::new();
    for (i, a) in artists.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&a.name);
    }
    s
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PlaylistItem {
    #[serde(default)]
    pub track: Option<Track>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Owner {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TracksRef {
    #[serde(default)]
    pub total: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Playlist {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub images: Option<Vec<Image>>,
    #[serde(default)]
    pub owner: Owner,
    #[serde(default)]
    pub tracks: Option<TracksRef>,
    #[serde(default)]
    pub public: Option<bool>,
    #[serde(default)]
    pub collaborative: Option<bool>,
    #[serde(default)]
    pub followers: Option<Followers>,
    /// Versión de la playlist según la Web API (/me/playlists la trae sin pedirla aparte):
    /// cambia con cada edición. Comparada con la de su copia en disco dice si hay que bajarla.
    #[serde(default)]
    pub snapshot_id: Option<String>,
}

impl Playlist {
    pub fn cover(&self, want: u32) -> Option<&str> {
        self.images.as_deref().and_then(|i| pick_image(i, want))
    }
    pub fn owner_name(&self) -> &str {
        self.owner.display_name.as_deref().unwrap_or("")
    }
    /// Sin portada, o con una armada aquí con su lista (`mosaic_cover`): se puede (re)hacer
    /// con las pistas. Una subida o la de la Web API no se tocan.
    pub fn own_cover(&self) -> bool {
        match self.images.as_deref() {
            None | Some([]) => true,
            Some([im]) => im.url.starts_with("spotify:mosaic:") || im.url.starts_with("spotify:image:"),
            Some(_) => false,
        }
    }
}

/// Portada de una playlist sin imagen subida, como la arma Spotify: mosaico con las portadas de
/// los cuatro primeros álbumes distintos de su lista, o la del primero si hay menos. En las
/// formas que entiende `App::cover_in` (`spotify:mosaic:…` / `spotify:image:…`), que además la
/// distinguen de una portada de verdad (`Playlist::own_cover`).
pub fn mosaic_cover(tracks: &[Track]) -> Option<Image> {
    let mut ids: Vec<&str> = Vec::new();
    for t in tracks {
        let Some(id) = t.cover(300).and_then(|u| u.strip_prefix("https://i.scdn.co/image/")) else { continue };
        // Solo ids limpios: van separados por «:» dentro del uri del mosaico.
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) || ids.contains(&id) {
            continue;
        }
        ids.push(id);
        if ids.len() == 4 {
            break;
        }
    }
    let url = match ids.as_slice() {
        [] => return None,
        [a, b, c, d] => format!("spotify:mosaic:{a}:{b}:{c}:{d}"),
        [first, ..] => format!("spotify:image:{first}"),
    };
    Some(Image { url, width: Some(300), height: Some(300) })
}

/// Listado de la biblioteca sacado del rootlist, completado con el anterior (la instantánea o el
/// de antes en esta sesión) en lo que el rootlist no trae: portada de las que no tienen una
/// subida, nombre visible del propietario, privacidad y seguidores. Orden y pertenencia son los
/// del rootlist. El snapshot_id nunca se hereda: uno viejo daría por buena una copia en disco
/// de antes de un cambio hecho en el móvil.
pub fn merge_rootlist_listing(old: &[Playlist], new: Vec<Playlist>, my_id: Option<&str>, my_name: Option<&str>) -> Vec<Playlist> {
    let by_id: std::collections::HashMap<&str, &Playlist> = old.iter().map(|p| (p.id.as_str(), p)).collect();
    new.into_iter()
        .map(|mut p| {
            if let Some(o) = by_id.get(p.id.as_str()) {
                // Sin atributos en el rootlist (sin nombre): nombre, descripción y colaboración,
                // los de antes. Con ellos manda el rootlist (una descripción vacía es que la borró).
                if p.name.is_empty() {
                    p.name = o.name.clone();
                    p.description = o.description.clone();
                }
                if p.images.as_ref().is_none_or(|v| v.is_empty()) {
                    p.images = o.images.clone();
                }
                match (&p.owner.id, &o.owner.id) {
                    (None, _) => p.owner = o.owner.clone(),
                    (Some(a), Some(b)) if a == b && p.owner.display_name.is_none() => {
                        p.owner.display_name = o.owner.display_name.clone();
                    }
                    _ => {}
                }
                p.tracks = p.tracks.or_else(|| o.tracks.clone());
                p.public = p.public.or(o.public);
                p.collaborative = p.collaborative.or(o.collaborative);
                p.followers = p.followers.take().or_else(|| o.followers.clone());
            }
            // Las propias se ven «De <tu nombre>» sin esperar a la Web API.
            if p.owner.display_name.is_none() && my_id.is_some() && p.owner.id.as_deref() == my_id {
                p.owner.display_name = my_name.map(str::to_string);
            }
            p
        })
        .collect()
}

/// Completa el listado del rootlist con el de la Web API (/me/playlists): portada (también los
/// mosaicos que genera Spotify), nombre visible del propietario y privacidad; nombre y tamaño
/// solo si faltan (los del rootlist son igual de frescos). El snapshot_id no: esta respuesta
/// puede ser de antes de una edición hecha mientras llegaba (que lo dejó vacío a propósito) y
/// daría por buena la copia en disco de antes del cambio. No cambia el orden ni quita ni añade
/// playlists. Devuelve cuántas completó.
pub fn enrich_listing(list: &mut [Playlist], web: Vec<Playlist>) -> usize {
    let mut by_id: std::collections::HashMap<String, Playlist> = web.into_iter().map(|p| (p.id.clone(), p)).collect();
    let mut n = 0;
    for p in list.iter_mut() {
        let Some(w) = by_id.remove(&p.id) else { continue };
        n += 1;
        if w.images.as_ref().is_some_and(|v| !v.is_empty()) {
            p.images = w.images;
        }
        if p.name.is_empty() {
            p.name = w.name;
        }
        if w.owner.display_name.is_some() && (p.owner.id.is_none() || p.owner.id == w.owner.id) {
            p.owner.display_name = w.owner.display_name;
        }
        p.owner.id = p.owner.id.take().or(w.owner.id);
        p.public = w.public.or(p.public);
        p.collaborative = p.collaborative.or(w.collaborative);
        p.tracks = p.tracks.take().or(w.tracks);
    }
    n
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Paging<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SavedTrack {
    #[serde(default)]
    pub track: Option<Track>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SavedAlbum {
    pub album: Album,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Album {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub artists: Vec<ArtistRef>,
    /// Géneros (metadatos internos; la Web API ya no los devuelve).
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub total_tracks: Option<u32>,
    #[serde(default)]
    pub album_type: Option<String>,
    #[serde(default)]
    pub tracks: Option<Paging<Track>>,
}

impl Album {
    pub fn year(&self) -> &str {
        year_of(self.release_date.as_deref())
    }
    pub fn artists_str(&self) -> String {
        join_artists(&self.artists)
    }
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }
    pub fn to_ref(&self) -> AlbumRef {
        AlbumRef {
            id: Some(self.id.clone()),
            name: self.name.clone(),
            uri: Some(self.uri.clone()),
            images: self.images.clone(),
            artists: self.artists.clone(),
            release_date: self.release_date.clone(),
            total_tracks: self.total_tracks,
            album_type: self.album_type.clone(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Followers {
    #[serde(default)]
    pub total: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Artist {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub followers: Option<Followers>,
}

impl Artist {
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TopTracks {
    #[serde(default)]
    pub tracks: Vec<Track>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SearchResult {
    #[serde(default)]
    pub tracks: Option<Paging<Track>>,
    #[serde(default)]
    pub albums: Option<Paging<Option<AlbumRef>>>,
    #[serde(default)]
    pub artists: Option<Paging<Option<Artist>>>,
    #[serde(default)]
    pub playlists: Option<Paging<Option<Playlist>>>,
    #[serde(default)]
    pub shows: Option<Paging<Option<Show>>>,
    #[serde(default)]
    pub episodes: Option<Paging<Option<Episode>>>,
    #[serde(default)]
    pub audiobooks: Option<Paging<Option<Audiobook>>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Show {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub total_episodes: Option<u32>,
    #[serde(default)]
    pub media_type: Option<String>,
    /// Temas del podcast (solo por los metadatos internos).
    #[serde(default)]
    pub keywords: Vec<String>,
}

impl Show {
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Episode {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub duration_ms: u32,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub explicit: bool,
}

impl Episode {
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }

    /// Representación como pista para reutilizar la tabla de canciones.
    pub fn as_track(&self, show: &str) -> Track {
        Track {
            id: Some(self.id.clone()),
            uri: self.uri.clone(),
            name: self.name.clone(),
            duration_ms: self.duration_ms,
            explicit: self.explicit,
            artists: vec![ArtistRef { id: None, name: show.to_string(), uri: None }],
            album: Some(AlbumRef {
                id: None,
                name: self.release_date.clone().unwrap_or_default(),
                uri: None,
                images: self.images.clone(),
                artists: Vec::new(),
                release_date: self.release_date.clone(),
                total_tracks: None,
                album_type: None,
            }),
            is_local: false,
            track_number: None,
            is_playable: None,
            kind: Some("episode".to_string()),
            added_by: None,
            added_at: None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Author {
    #[serde(default)]
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Audiobook {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub authors: Vec<Author>,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub description: String,
}

impl Audiobook {
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }
    pub fn authors_str(&self) -> String {
        self.authors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    }
}

/// Vista completa de un artista por los metadatos internos.
#[derive(Clone, Debug, Default)]
pub struct ArtistView {
    /// Nombre (por si la Web API no responde: la cabecera no se queda en «Artista»).
    pub name: String,
    pub header: Option<String>,
    pub biography: String,
    pub related: Vec<Artist>,
    pub albums: Vec<AlbumRef>,
    pub singles: Vec<AlbumRef>,
    pub compilations: Vec<AlbumRef>,
    pub appears_on: Vec<AlbumRef>,
    pub latest: Option<AlbumRef>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Device {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_active: bool,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default, deserialize_with = "lenient_u32")]
    pub volume_percent: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Devices {
    #[serde(default)]
    pub devices: Vec<Device>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Context {
    #[serde(default)]
    pub uri: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PlaybackState {
    #[serde(default)]
    pub device: Option<Device>,
    #[serde(default)]
    pub is_playing: bool,
    #[serde(default, deserialize_with = "lenient_u32")]
    pub progress_ms: Option<u32>,
    #[serde(default)]
    pub item: Option<Track>,
    #[serde(default)]
    pub shuffle_state: bool,
    #[serde(default)]
    pub repeat_state: String,
    #[serde(default)]
    pub context: Option<Context>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct PlayHistory {
    #[serde(default)]
    pub track: Option<Track>,
    #[serde(default)]
    pub played_at: Option<String>,
    #[serde(default)]
    pub context: Option<Context>,
}

/// Última actividad conocida en la cuenta (del historial del servidor): sirve para decidir si la
/// sesión del servidor es más reciente que la copia local guardada al cerrar Nanofy.
#[derive(Clone, Debug)]
pub struct ServerLast {
    pub played_at: u64,
    pub context_uri: Option<String>,
    pub track_uri: String,
    pub track: Option<Track>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct User {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub images: Vec<Image>,
}

impl User {
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }
}

/// Pista en reproducción, independiente de si suena aquí o en otro dispositivo.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NowPlaying {
    pub uri: String,
    pub id: Option<String>,
    pub name: String,
    /// (nombre, id)
    pub artists: Vec<(String, Option<String>)>,
    pub album: String,
    pub album_id: Option<String>,
    pub cover_url: Option<String>,
    pub duration_ms: u32,
}

impl NowPlaying {
    pub fn from_track(t: &Track) -> Self {
        Self {
            uri: t.uri.clone(),
            id: t.id.clone(),
            name: t.name.clone(),
            artists: t
                .artists
                .iter()
                .map(|a| (a.name.clone(), a.id.clone()))
                .collect(),
            album: t.album_name().to_string(),
            album_id: t.album.as_ref().and_then(|a| a.id.clone()),
            cover_url: t.cover(300).map(|s| s.to_string()),
            duration_ms: t.duration_ms,
        }
    }
    pub fn artists_str(&self) -> String {
        self.artists
            .iter()
            .map(|a| a.0.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

// ------------------------------------------------------------------ audio que suena

/// Frecuencia a la que el reproductor entrega el audio a la salida (la de Spotify).
pub const SOURCE_RATE: u32 = 44_100;

/// Lo que suena de verdad en este equipo: formato, calidad y normalización de la canción. Lo
/// manda el reproductor al empezar cada canción y cuando la normalización cambia en vivo; lo
/// enseñan la etiqueta de calidad de la barra y el modo de control.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioInfo {
    /// Pista a la que corresponde (la `uri` de `NowPlaying`): la de otra pista no se enseña.
    pub uri: String,
    /// Nombre del formato de Spotify («OGG_VORBIS_320»); `None` en archivos locales.
    pub format: Option<String>,
    /// Códec legible: «Ogg Vorbis», «MP3»…
    pub codec: String,
    pub kbps: Option<u16>,
    /// Bits por muestra del original (solo formatos sin pérdida de archivos locales).
    pub bits: Option<u32>,
    pub sample_rate: u32,
    /// kbps de la calidad pedida al cargarla; `None` en archivos locales.
    pub requested_kbps: Option<u16>,
    /// Suena una copia de la caché en otra calidad aunque la pedida existe.
    pub from_cache: bool,
    /// Episodio de pódcast: casi todos están solo a 96 kbps y eso no merece un aviso.
    pub episode: bool,
    /// Ganancia de la normalización aplicada, en dB; `None` con la normalización apagada.
    pub gain_db: Option<f32>,
    /// La ganancia es la del álbum (un álbum en orden), no la de la canción.
    pub album_gain: bool,
    /// La canción trae datos de volumen; sin ellos la ganancia es 0 dB.
    pub gain_data: bool,
}

impl AudioInfo {
    /// Texto de la etiqueta de la barra: los kbps o, sin ellos, el códec.
    pub fn badge(&self) -> String {
        match self.kbps {
            Some(k) => format!("{k} kbps"),
            None => self.codec.clone(),
        }
    }

    /// Suena por debajo de la calidad que se pidió al cargarla (la etiqueta se pinta en ámbar).
    /// Los pódcasts a 96 kbps son lo normal: casi ninguno tiene más.
    pub fn below_requested(&self) -> bool {
        !self.episode && matches!((self.kbps, self.requested_kbps), (Some(k), Some(r)) if k < r)
    }

    /// «Ogg Vorbis · 320 kbps · 44,1 kHz».
    pub fn source_line(&self) -> String {
        let mut parts = vec![self.codec.clone()];
        if let Some(k) = self.kbps {
            parts.push(format!("{k} kbps"));
        }
        if let Some(b) = self.bits {
            parts.push(format!("{b} bit"));
        }
        if self.sample_rate > 0 {
            parts.push(format!("{} kHz", fmt_khz(self.sample_rate)));
        }
        parts.join(" · ")
    }

    /// «Normalización: Normal · −5,3 dB (por canción)». `level` es el nombre del nivel elegido.
    pub fn normalisation_line(&self, level: &str) -> String {
        match self.gain_db {
            None => "Normalización desactivada".to_string(),
            Some(_) if !self.gain_data => format!("Normalización: {level} · sin datos de volumen (0 dB)"),
            Some(db) => format!(
                "Normalización: {level} · {} ({})",
                fmt_db(db),
                if self.album_gain { "por álbum" } else { "por canción" }
            ),
        }
    }

    /// Líneas del globo de la etiqueta. `chosen_kbps`: la calidad de los ajustes ahora mismo (si
    /// se cambió con la canción ya sonando, vale desde la siguiente).
    pub fn tooltip(&self, output: AudioOutput, level: &str, chosen_kbps: u16) -> Vec<String> {
        let mut lines = vec![self.source_line()];
        if let Some(o) = output.summary(false) {
            lines.push(format!("Salida: {o}"));
        }
        lines.push(self.normalisation_line(level));
        if self.below_requested() {
            if let (Some(k), Some(r)) = (self.kbps, self.requested_kbps) {
                lines.push(if self.from_cache {
                    format!("Suena la copia ya guardada a {k} kbps (de antes de cambiar la calidad).")
                } else {
                    format!("Esta canción no está disponible en {r} kbps; suena a {k} kbps.")
                });
            }
        }
        if self.requested_kbps.is_some_and(|r| r != chosen_kbps) {
            lines.push(format!("La calidad nueva ({chosen_kbps} kbps) se aplica desde la próxima canción."));
        }
        lines
    }
}

/// Quién pasa el audio de 44,1 kHz a la frecuencia del dispositivo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resampling {
    /// El dispositivo ya está a 44,1 kHz: el audio llega tal cual.
    None,
    /// Nuestro remuestreador sinc polifásico.
    Sinc,
    /// El conversor lineal de rodio (NANOFY_RESAMPLER=rodio o una relación no soportada).
    Rodio,
}

impl Resampling {
    /// Nombre para el modo de control.
    pub fn name(self) -> &'static str {
        match self {
            Resampling::None => "none",
            Resampling::Sinc => "sinc",
            Resampling::Rodio => "rodio",
        }
    }
}

/// Cómo sale el audio hacia el dispositivo (`audio_backend::output_info`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioOutput {
    /// Frecuencia del dispositivo en Hz; 0 si aún no se ha abierto ninguna salida.
    pub rate: u32,
    pub channels: u16,
    /// El paso de 44,1 kHz a `rate` lo hace nuestro remuestreador sinc.
    pub sinc: bool,
}

impl AudioOutput {
    /// `None` mientras no se haya abierto ninguna salida (no ha sonado nada todavía).
    pub fn resampling(&self) -> Option<Resampling> {
        match self.rate {
            0 => None,
            SOURCE_RATE => Some(Resampling::None),
            _ if self.sinc => Some(Resampling::Sinc),
            _ => Some(Resampling::Rodio),
        }
    }

    /// «48 kHz · remuestreo de alta calidad» o «44,1 kHz · sin remuestreo»; con `with_ratio`,
    /// también de qué frecuencia a cuál. `None` si aún no se ha abierto ninguna salida.
    pub fn summary(&self, with_ratio: bool) -> Option<String> {
        let resampling = self.resampling()?;
        let mut s = format!("{} kHz · ", fmt_khz(self.rate));
        s.push_str(match resampling {
            Resampling::None => "sin remuestreo",
            Resampling::Sinc => "remuestreo de alta calidad",
            Resampling::Rodio => "remuestreo básico de rodio",
        });
        if with_ratio && resampling != Resampling::None {
            s.push_str(&format!(" ({} → {} kHz)", fmt_khz(SOURCE_RATE), fmt_khz(self.rate)));
        }
        if self.channels == 1 {
            s.push_str(" · mono");
        }
        Some(s)
    }
}

/// 44100 → «44,1»; 48000 → «48»; 22050 → «22,05» (coma decimal, sin ceros de sobra).
pub fn fmt_khz(hz: u32) -> String {
    let s = format!("{:.2}", hz as f64 / 1000.0);
    s.trim_end_matches('0').trim_end_matches('.').replace('.', ",")
}

/// Ganancia con una cifra decimal, coma y signo tipográfico: «−5,3 dB», «+2,0 dB», «0 dB».
pub fn fmt_db(db: f32) -> String {
    let r = (db * 10.0).round() / 10.0;
    if r == 0.0 || !r.is_finite() {
        return "0 dB".to_string();
    }
    let sign = if r < 0.0 { '−' } else { '+' };
    format!("{sign}{:.1} dB", r.abs()).replace('.', ",")
}

pub fn fmt_ms(ms: u32) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "2024-05-10" → "10 may 2024" (también acepta "2024-05" y "2024").
pub fn fmt_date(date: &str) -> String {
    const MESES: [&str; 12] = ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"];
    let parts: Vec<&str> = date.split('-').collect();
    let month = parts.get(1).and_then(|m| m.parse::<usize>().ok()).filter(|m| (1..=12).contains(m));
    match (parts.first(), month, parts.get(2)) {
        (Some(y), Some(m), Some(d)) => format!("{} {} {y}", d.trim_start_matches('0'), MESES[m - 1]),
        (Some(y), Some(m), None) => format!("{} {y}", MESES[m - 1]),
        (Some(y), _, _) => y.to_string(),
        _ => date.to_string(),
    }
}

pub fn fmt_total(ms: u64) -> String {
    let mins = ms / 60_000;
    if mins >= 60 {
        format!("{} h {} min", mins / 60, mins % 60)
    } else {
        format!("{mins} min")
    }
}

// ------------------------------------------------------------------ perfiles y seguimiento

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UserProfile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub images: Vec<Image>,
    #[serde(default)]
    pub followers: Option<Followers>,
}

impl UserProfile {
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.id)
    }
    pub fn cover(&self, want: u32) -> Option<&str> {
        pick_image(&self.images, want)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Cursors {
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CursorPaging<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub cursors: Option<Cursors>,
    #[serde(default)]
    pub total: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FollowedArtists {
    #[serde(default)]
    pub artists: CursorPaging<Artist>,
}

// ------------------------------------------------------------------ cola

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct QueueResponse {
    #[serde(default)]
    pub currently_playing: Option<Track>,
    #[serde(default)]
    pub queue: Vec<Track>,
}

// ------------------------------------------------------------------ letras

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LyricLine {
    pub start_ms: u32,
    pub words: String,
    /// Tiempos por sílaba (o por palabra), si la letra los trae: cada una empieza en su
    /// `start_ms` y abarca sus `chars` caracteres de `words`, a continuación de la anterior. La
    /// mayoría de las letras solo traen el comienzo de cada renglón (vacío).
    pub syllables: Vec<Syllable>,
    /// Final del renglón, si la letra lo da.
    pub end_ms: Option<u32>,
}

/// Una sílaba de un renglón con tiempos por sílaba.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Syllable {
    pub start_ms: u32,
    pub chars: usize,
}

/// Milisegundos de un campo que Spotify da como texto o como número.
fn ms_field(v: Option<&serde_json::Value>) -> Option<u32> {
    let v = v?;
    v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_u64().map(|n| n as u32))
}

/// `mm:ss.xx` → milisegundos.
fn lrc_stamp(stamp: &str) -> Option<u32> {
    let (m, s) = stamp.split_once(':')?;
    let (m, s) = (m.trim().parse::<u32>().ok()?, s.trim().parse::<f32>().ok()?);
    Some(m * 60_000 + (s * 1000.0).round() as u32)
}

/// Un renglón de LRC «mejorado» (`<mm:ss.xx>` delante de cada palabra): el texto sin las marcas y
/// los tiempos de cada trozo. Sin marcas, el texto tal cual y sin sílabas. Los espacios a los dos
/// lados de una marca quedan en uno, y los del principio, fuera (no se pintan).
fn lrc_words(text: &str) -> (String, Vec<Syllable>) {
    if !text.contains('<') {
        return (text.trim().to_string(), Vec::new());
    }
    let mut words = String::new();
    let mut syl: Vec<Syllable> = Vec::new();
    // Añade un trozo de texto a la última sílaba.
    let push = |words: &mut String, syl: &mut Vec<Syllable>, piece: &str| {
        let piece = if words.is_empty() || words.ends_with(char::is_whitespace) { piece.trim_start() } else { piece };
        if let Some(last) = syl.last_mut() {
            last.chars += piece.chars().count();
        }
        words.push_str(piece);
    };
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        push(&mut words, &mut syl, &rest[..i]);
        let Some(j) = rest[i..].find('>') else { break };
        match lrc_stamp(&rest[i + 1..i + j]) {
            Some(t) => syl.push(Syllable { start_ms: t, chars: 0 }),
            None => push(&mut words, &mut syl, &rest[i..i + j + 1]),
        }
        rest = &rest[i + j + 1..];
    }
    push(&mut words, &mut syl, rest);
    syl.retain(|s| s.chars > 0);
    (words.trim_end().to_string(), syl)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Lyrics {
    pub track_id: String,
    /// "LINE_SYNCED", "SYLLABLE_SYNCED" (con tiempos por sílaba) o "UNSYNCED"
    pub sync_type: String,
    pub lines: Vec<LyricLine>,
    pub provider: String,
}

impl Lyrics {
    /// Con tiempos (por renglón o por sílaba).
    pub fn synced(&self) -> bool {
        self.sync_type == "LINE_SYNCED" || self.sync_type == "SYLLABLE_SYNCED"
    }

    pub fn from_json(track_id: &str, v: &serde_json::Value) -> Option<Self> {
        let l = v.get("lyrics")?;
        let lines = l
            .get("lines")?
            .as_array()?
            .iter()
            .map(|line| LyricLine {
                start_ms: ms_field(line.get("startTimeMs")).unwrap_or(0),
                words: line
                    .get("words")
                    .and_then(|w| w.as_str())
                    .unwrap_or("")
                    .to_string(),
                syllables: line
                    .get("syllables")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| {
                                let chars = s.get("numChars").and_then(|n| n.as_u64().or_else(|| n.as_str().and_then(|t| t.parse().ok())))?;
                                Some(Syllable { start_ms: ms_field(s.get("startTimeMs"))?, chars: chars as usize })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                end_ms: ms_field(line.get("endTimeMs")).filter(|&e| e > 0),
            })
            .collect();
        Some(Self {
            track_id: track_id.to_string(),
            sync_type: l
                .get("syncType")
                .and_then(|s| s.as_str())
                .unwrap_or("UNSYNCED")
                .to_string(),
            lines,
            provider: l
                .get("providerDisplayName")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }

    /// Formato LRC: líneas `[mm:ss.xx] texto`.
    pub fn from_lrc(track_id: &str, lrc: &str, provider: &str) -> Self {
        let mut lines = Vec::new();
        for raw in lrc.lines() {
            let raw = raw.trim();
            let Some(rest) = raw.strip_prefix('[') else { continue };
            let Some((stamp, text)) = rest.split_once(']') else { continue };
            let Some(start_ms) = lrc_stamp(stamp) else { continue };
            let (words, syllables) = lrc_words(text);
            lines.push(LyricLine { start_ms, words, syllables, end_ms: None });
        }
        lines.sort_by_key(|l| l.start_ms);
        Self {
            track_id: track_id.to_string(),
            sync_type: "LINE_SYNCED".to_string(),
            lines,
            provider: provider.to_string(),
        }
    }

    pub fn from_plain(track_id: &str, text: &str, provider: &str) -> Self {
        Self {
            track_id: track_id.to_string(),
            sync_type: "UNSYNCED".to_string(),
            lines: text
                .lines()
                .map(|l| LyricLine {
                    start_ms: 0,
                    words: l.trim().to_string(),
                    ..Default::default()
                })
                .collect(),
            provider: provider.to_string(),
        }
    }

    /// Índice de la línea activa para una posición dada.
    pub fn current_line(&self, position_ms: u32) -> Option<usize> {
        if !self.synced() {
            return None;
        }
        let mut idx = None;
        for (i, line) in self.lines.iter().enumerate() {
            if line.start_ms <= position_ms {
                idx = Some(i);
            } else {
                break;
            }
        }
        idx
    }
}

// ------------------------------------------------------------------ Jam (sesión social)

#[derive(Clone, Debug, Default, PartialEq)]
pub struct JamMember {
    pub id: String,
    pub name: String,
    pub image_url: Option<String>,
    pub is_owner: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct JamSession {
    pub session_id: String,
    pub join_token: String,
    pub join_url: String,
    pub is_owner: bool,
    pub members: Vec<JamMember>,
    pub max_members: u32,
}

impl JamSession {
    pub fn from_json(v: &serde_json::Value) -> Option<Self> {
        let session_id = v.get("session_id")?.as_str()?.to_string();
        let owner = v
            .get("session_owner_id")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let members = v
            .get("session_members")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|m| {
                        let id = m.get("id").and_then(|s| s.as_str()).unwrap_or("").to_string();
                        JamMember {
                            is_owner: !owner.is_empty() && id == owner,
                            name: m
                                .get("display_name")
                                .and_then(|s| s.as_str())
                                .or_else(|| m.get("username").and_then(|s| s.as_str()))
                                .unwrap_or("?")
                                .to_string(),
                            image_url: m
                                .get("image_url")
                                .and_then(|s| s.as_str())
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string()),
                            id,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let join_token = v
            .get("join_session_token")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        // Spotify devuelve `join_session_url` como URI interna (hm://social-connect/...), que no
        // sirve para compartir: el enlace público es siempre open.spotify.com/socialsession/<token>.
        let join_url = v
            .get("join_session_url")
            .and_then(|s| s.as_str())
            .filter(|s| s.starts_with("https://"))
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("https://open.spotify.com/socialsession/{join_token}"));
        Some(Self {
            session_id,
            join_token,
            join_url,
            is_owner: v
                .get("is_session_owner")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
            members,
            max_members: v
                .get("maximum_session_members")
                .and_then(|n| n.as_u64())
                .unwrap_or(0) as u32,
        })
    }
}

/// Extrae el token de un enlace o URI de Jam (`https://open.spotify.com/socialsession/<token>`).
pub fn jam_token_from_link(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let s = s.split(['?', '#']).next().unwrap_or(s);
    let token = s.rsplit(['/', ':']).next().unwrap_or(s);
    (!token.is_empty()).then(|| token.to_string())
}

/// Convierte un enlace `open.spotify.com/...` o un uri `spotify:...` en (tipo, id).
pub fn parse_spotify_link(input: &str) -> Option<(String, String)> {
    let s = input.trim();
    if let Some(rest) = s.strip_prefix("spotify:") {
        let mut parts = rest.split(':');
        let kind = parts.next()?.to_string();
        let id = parts.next()?.to_string();
        return Some((kind, id));
    }
    let idx = s.find("open.spotify.com/")?;
    let path = &s[idx + "open.spotify.com/".len()..];
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let mut segs = path.split('/').filter(|p| !p.is_empty());
    let mut kind = segs.next()?.to_string();
    // enlaces con locale: open.spotify.com/intl-es/track/...
    if kind.starts_with("intl-") {
        kind = segs.next()?.to_string();
    }
    let id = segs.next()?.to_string();
    Some((kind, id))
}

/// Estantería de la página de inicio (endpoint interno homeview).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct HomeSection {
    pub id: String,
    pub title: String,
    pub items: Vec<HomeItem>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct HomeItem {
    pub uri: String,
    pub title: String,
    pub subtitle: String,
    /// URL http, `spotify:image:<id>` o `spotify:mosaic:<id>:<id>:<id>:<id>`.
    pub image: Option<String>,
    /// Contexto de reproducción (playlist) para ítems que son canciones.
    pub context: Option<String>,
}

impl HomeItem {
    /// "playlist", "album", "artist", "show", "collection" (Me gusta) u otro.
    pub fn kind(&self) -> &str {
        if self.uri.ends_with(":collection") {
            return "collection";
        }
        self.uri.split(':').nth(1).unwrap_or("")
    }
    pub fn id(&self) -> &str {
        self.uri.rsplit(':').next().unwrap_or("")
    }
}

impl HomeSection {
    /// Filas de recomendaciones (frente a las basadas en tu biblioteca).
    pub fn is_recommendation(&self) -> bool {
        let t = self.title.to_lowercase();
        ["más como", "para fans de", "descubre más de", "recomendad", "emisoras recomendadas", "deja que", "álbumes con canciones"]
            .iter()
            .any(|p| t.starts_with(p))
            || t.contains("recomend")
    }

    /// 0 música, 1 podcasts, 2 audiolibros (por el título y el tipo de los ítems).
    pub fn category(&self) -> u8 {
        let t = self.title.to_lowercase();
        if t.contains("audiolibro") || t.contains("audiobook") {
            return 2;
        }
        let shows = self.items.iter().filter(|i| i.kind() == "show").count();
        if !self.items.is_empty() && shows * 2 > self.items.len() {
            1
        } else {
            0
        }
    }
}

/// Carpeta de playlists (del rootlist interno; la Web API no las expone).
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub playlists: Vec<String>,
}

/// Episodio guardado ("Tus episodios") con su podcast.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct SavedEpisode {
    pub episode: Episode,
    pub show_name: String,
    pub show_id: String,
}

/// Forma de /me/episodes: el episodio lleva el podcast anidado.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct EpisodeWithShow {
    #[serde(flatten)]
    pub episode: Episode,
    pub show: Option<Show>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estado_del_reproductor_con_numeros_raros() {
        // Spotify manda a veces un progreso negativo al empezar una canción: antes se perdía
        // todo el estado («invalid value: integer `-86`, expected u32»).
        let st: PlaybackState = serde_json::from_str(
            r#"{"device":{"id":"d","name":"PC","volume_percent":55.0},"is_playing":true,"progress_ms":-86}"#,
        )
        .unwrap();
        assert_eq!(st.progress_ms, Some(0));
        assert_eq!(st.device.unwrap().volume_percent, Some(55));
        let st: PlaybackState = serde_json::from_str(r#"{"progress_ms":null}"#).unwrap();
        assert_eq!(st.progress_ms, None);
        let st: PlaybackState = serde_json::from_str(r#"{"progress_ms":123456}"#).unwrap();
        assert_eq!(st.progress_ms, Some(123_456));
        let st: PlaybackState = serde_json::from_str("{}").unwrap();
        assert_eq!(st.progress_ms, None);
    }

    fn ogg(kbps: u16, requested: u16) -> AudioInfo {
        AudioInfo {
            uri: "spotify:track:x".into(),
            format: Some(format!("OGG_VORBIS_{kbps}")),
            codec: "Ogg Vorbis".into(),
            kbps: Some(kbps),
            sample_rate: SOURCE_RATE,
            requested_kbps: Some(requested),
            gain_db: Some(-5.3),
            gain_data: true,
            ..Default::default()
        }
    }

    #[test]
    fn frecuencias_y_ganancias_en_espanol() {
        assert_eq!(fmt_khz(44_100), "44,1");
        assert_eq!(fmt_khz(48_000), "48");
        assert_eq!(fmt_khz(88_200), "88,2");
        assert_eq!(fmt_khz(96_000), "96");
        assert_eq!(fmt_khz(22_050), "22,05");
        assert_eq!(fmt_khz(192_000), "192");
        assert_eq!(fmt_db(-5.3), "−5,3 dB");
        assert_eq!(fmt_db(-5.26), "−5,3 dB");
        assert_eq!(fmt_db(2.0), "+2,0 dB");
        assert_eq!(fmt_db(0.0), "0 dB");
        assert_eq!(fmt_db(-0.04), "0 dB");
        assert_eq!(fmt_db(f32::NAN), "0 dB");
    }

    /// Ámbar solo si suena por debajo de lo que se pidió al cargarla: no con la caché en más
    /// calidad, ni en pódcasts (casi todos solo a 96), ni en archivos locales.
    #[test]
    fn por_debajo_de_la_calidad_pedida() {
        assert!(!ogg(320, 320).below_requested());
        assert!(ogg(160, 320).below_requested());
        assert!(!ogg(320, 160).below_requested());
        assert!(!AudioInfo { episode: true, ..ogg(96, 320) }.below_requested());
        assert!(!AudioInfo { requested_kbps: None, ..ogg(96, 320) }.below_requested());
        assert!(!AudioInfo { kbps: None, ..ogg(96, 320) }.below_requested());
    }

    #[test]
    fn etiqueta_y_globo() {
        let out48 = AudioOutput { rate: 48_000, channels: 2, sinc: true };
        let out44 = AudioOutput { rate: 44_100, channels: 2, sinc: false };
        let a = ogg(320, 320);
        assert_eq!(a.badge(), "320 kbps");
        assert_eq!(
            a.tooltip(out48, "Normal", 320),
            vec![
                "Ogg Vorbis · 320 kbps · 44,1 kHz",
                "Salida: 48 kHz · remuestreo de alta calidad",
                "Normalización: Normal · −5,3 dB (por canción)",
            ]
        );
        assert_eq!(a.tooltip(out44, "Normal", 320)[1], "Salida: 44,1 kHz · sin remuestreo");
        // Sin salida abierta todavía, la línea no sale.
        assert_eq!(a.tooltip(AudioOutput::default(), "Normal", 320).len(), 2);
        // Álbum, sin datos y apagada.
        let album = AudioInfo { album_gain: true, gain_db: Some(1.25), ..ogg(320, 320) };
        assert_eq!(album.normalisation_line("Alto"), "Normalización: Alto · +1,3 dB (por álbum)");
        let no_data = AudioInfo { gain_data: false, gain_db: Some(0.0), ..ogg(320, 320) };
        assert_eq!(no_data.normalisation_line("Bajo"), "Normalización: Bajo · sin datos de volumen (0 dB)");
        let off = AudioInfo { gain_db: None, ..ogg(320, 320) };
        assert_eq!(off.normalisation_line("Normal"), "Normalización desactivada");
        // La canción no existe en 320: se dice. Si es la copia de la caché, se dice eso otro.
        let low = ogg(160, 320);
        assert_eq!(
            low.tooltip(out48, "Normal", 320).last().unwrap(),
            "Esta canción no está disponible en 320 kbps; suena a 160 kbps."
        );
        let cached = AudioInfo { from_cache: true, ..ogg(160, 320) };
        assert_eq!(
            cached.tooltip(out48, "Normal", 320).last().unwrap(),
            "Suena la copia ya guardada a 160 kbps (de antes de cambiar la calidad)."
        );
        // Calidad cambiada con la canción sonando: vale desde la siguiente (y no es un aviso).
        let before = ogg(160, 160);
        assert!(!before.below_requested());
        assert_eq!(
            before.tooltip(out48, "Normal", 320).last().unwrap(),
            "La calidad nueva (320 kbps) se aplica desde la próxima canción."
        );
        // Archivo local sin pérdida: códec, bits y frecuencia.
        let local = AudioInfo { codec: "Archivo local".into(), bits: Some(24), sample_rate: SOURCE_RATE, ..Default::default() };
        assert_eq!(local.badge(), "Archivo local");
        assert_eq!(local.source_line(), "Archivo local · 24 bit · 44,1 kHz");
    }

    #[test]
    fn salida_de_audio() {
        let sinc = AudioOutput { rate: 48_000, channels: 2, sinc: true };
        assert_eq!(sinc.resampling(), Some(Resampling::Sinc));
        assert_eq!(sinc.summary(true).unwrap(), "48 kHz · remuestreo de alta calidad (44,1 → 48 kHz)");
        let native = AudioOutput { rate: 44_100, channels: 2, sinc: false };
        assert_eq!(native.resampling(), Some(Resampling::None));
        assert_eq!(native.summary(true).unwrap(), "44,1 kHz · sin remuestreo");
        let rodio = AudioOutput { rate: 96_000, channels: 2, sinc: false };
        assert_eq!(rodio.resampling().map(Resampling::name), Some("rodio"));
        assert_eq!(rodio.summary(false).unwrap(), "96 kHz · remuestreo básico de rodio");
        let mono = AudioOutput { rate: 48_000, channels: 1, sinc: true };
        assert_eq!(mono.summary(false).unwrap(), "48 kHz · remuestreo de alta calidad · mono");
        assert_eq!(AudioOutput::default().resampling(), None);
        assert_eq!(AudioOutput::default().summary(true), None);
    }

    fn track_on(album: &str) -> Track {
        Track {
            uri: format!("spotify:track:{album}1"),
            album: Some(AlbumRef {
                id: Some(album.to_string()),
                images: vec![
                    Image { url: format!("https://i.scdn.co/image/{album}640"), width: Some(640), height: Some(640) },
                    Image { url: format!("https://i.scdn.co/image/{album}300"), width: Some(300), height: Some(300) },
                    Image { url: format!("https://i.scdn.co/image/{album}64"), width: Some(64), height: Some(64) },
                ],
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn pl(id: &str, name: &str) -> Playlist {
        Playlist { id: id.into(), name: name.into(), uri: format!("spotify:playlist:{id}"), ..Default::default() }
    }

    fn img(url: &str) -> Option<Vec<Image>> {
        Some(vec![Image { url: url.into(), width: Some(300), height: Some(300) }])
    }

    /// Como Spotify: cuatro primeros álbumes distintos en mosaico; con menos, el primero.
    #[test]
    fn mosaico_de_portadas() {
        let t = |a: &str| track_on(a);
        let four = [t("a"), t("a"), t("b"), t("c"), t("b"), t("d"), t("e")];
        let m = mosaic_cover(&four).unwrap();
        assert_eq!(m.url, "spotify:mosaic:a300:b300:c300:d300");
        let two = [t("a"), t("b"), t("a")];
        assert_eq!(mosaic_cover(&two).unwrap().url, "spotify:image:a300");
        // Sin portadas de i.scdn.co (locales, sin álbum): nada.
        assert!(mosaic_cover(&[Track::default()]).is_none());
        assert!(mosaic_cover(&[]).is_none());
        let mut odd = t("x");
        odd.album.as_mut().unwrap().images = vec![Image { url: "https://otra.cdn/x.jpg".into(), width: Some(300), height: None }];
        assert_eq!(mosaic_cover(&[odd, t("a")]).unwrap().url, "spotify:image:a300");
        // Lo armado aquí se puede rehacer; una portada de verdad no.
        let mut p = pl("p", "P");
        assert!(p.own_cover());
        p.images = Some(vec![m]);
        assert!(p.own_cover());
        p.images = img("https://i.scdn.co/image/abc");
        assert!(!p.own_cover());
        p.images = img("https://mosaic.scdn.co/300/a/b/c/d");
        assert!(!p.own_cover());
    }

    /// El listado del rootlist manda en orden, pertenencia y lo que trae; lo demás, del anterior.
    #[test]
    fn listado_del_rootlist_con_el_anterior() {
        let mut old_a = pl("a", "A vieja");
        old_a.images = img("https://mosaic.scdn.co/300/a");
        old_a.owner = Owner { display_name: Some("Otra Persona".into()), id: Some("otra".into()) };
        old_a.public = Some(false);
        old_a.snapshot_id = Some("viejo".into());
        old_a.followers = Some(Followers { total: Some(12) });
        let mut old_b = pl("b", "B");
        old_b.images = img("https://i.scdn.co/image/bb");
        old_b.description = Some("descripción de antes".into());
        let gone = pl("z", "Ya no está");

        let mut new_a = pl("a", "A nueva");
        new_a.owner.id = Some("otra".into());
        new_a.snapshot_id = Some("nuevo".into());
        // Sin atributos en el rootlist: nombre y descripción, los de antes.
        let mut new_b = pl("b", "");
        new_b.owner.id = Some("yo".into());
        let mut new_c = pl("c", "Mía nueva");
        new_c.owner.id = Some("yo".into());
        let mut new_d = pl("d", "De Spotify");
        new_d.owner = Owner { display_name: Some("Spotify".into()), id: Some("spotify".into()) };

        let out = merge_rootlist_listing(&[old_b, gone, old_a], vec![new_a, new_b, new_c, new_d], Some("yo"), Some("Yo Mismo"));
        let ids: Vec<&str> = out.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d"]);
        let a = &out[0];
        assert_eq!(a.name, "A nueva");
        assert_eq!(a.cover(300), Some("https://mosaic.scdn.co/300/a"));
        assert_eq!(a.owner_name(), "Otra Persona");
        assert_eq!(a.public, Some(false));
        assert_eq!(a.followers.as_ref().and_then(|f| f.total), Some(12));
        // El snapshot_id es siempre el del rootlist, nunca el heredado.
        assert_eq!(a.snapshot_id.as_deref(), Some("nuevo"));
        let b = &out[1];
        assert_eq!(b.name, "B");
        assert_eq!(b.description.as_deref(), Some("descripción de antes"));
        assert_eq!(b.cover(300), Some("https://i.scdn.co/image/bb"));
        assert_eq!(b.snapshot_id, None);
        // Las propias se ven con el nombre propio aunque sean nuevas.
        assert_eq!(b.owner_name(), "Yo Mismo");
        assert_eq!(out[2].owner_name(), "Yo Mismo");
        assert_eq!(out[2].public, None);
        assert_eq!(out[3].owner_name(), "Spotify");

        // Si cambió de propietario no se le pone el nombre visible del anterior.
        let mut moved = pl("a", "A");
        moved.owner.id = Some("nueva".into());
        let out = merge_rootlist_listing(&out, vec![moved], Some("yo"), None);
        assert_eq!(out[0].owner_name(), "");
        assert_eq!(out[0].owner.id.as_deref(), Some("nueva"));
    }

    /// La Web API completa portada, propietario y privacidad sin tocar orden, pertenencia ni el
    /// snapshot_id del rootlist.
    #[test]
    fn la_web_api_completa_el_listado() {
        let mut a = pl("a", "A");
        a.owner.id = Some("otra".into());
        a.snapshot_id = Some("rev".into());
        a.images = Some(vec![mosaic_cover(&[track_on("x")]).unwrap()]);
        let mut b = pl("b", "");
        b.images = img("https://i.scdn.co/image/subida");
        let c = pl("c", "C");
        let mut list = vec![a, b, c];

        let mut wa = pl("a", "A (web)");
        wa.images = img("https://mosaic.scdn.co/640/x");
        wa.owner = Owner { display_name: Some("Otra Persona".into()), id: Some("otra".into()) };
        wa.public = Some(true);
        wa.snapshot_id = Some("otro".into());
        wa.tracks = Some(TracksRef { total: 7 });
        let mut wb = pl("b", "B (web)");
        wb.images = Some(Vec::new());
        wb.public = Some(false);
        let extra = pl("w", "Solo en la Web API");

        assert_eq!(enrich_listing(&mut list, vec![extra, wb, wa]), 2);
        let ids: Vec<&str> = list.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        let a = &list[0];
        assert_eq!(a.name, "A");
        assert_eq!(a.cover(300), Some("https://mosaic.scdn.co/640/x"));
        assert_eq!(a.owner_name(), "Otra Persona");
        assert_eq!(a.public, Some(true));
        assert_eq!(a.snapshot_id.as_deref(), Some("rev"));
        assert_eq!(a.tracks.as_ref().map(|t| t.total), Some(7));
        let b = &list[1];
        // Nombre solo si faltaba; sin portada en la Web API se queda la que tenía.
        assert_eq!(b.name, "B (web)");
        assert_eq!(b.cover(300), Some("https://i.scdn.co/image/subida"));
        assert_eq!(b.public, Some(false));
        assert_eq!(b.snapshot_id, None);
        assert_eq!(list[2].public, None);
    }

    /// Letra de Spotify con tiempos por sílaba: cuenta como sincronizada y cada renglón trae sus
    /// sílabas (números como texto o como número); un final 0 es «sin final».
    #[test]
    fn letra_con_silabas_de_spotify() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"lyrics":{"syncType":"SYLLABLE_SYNCED","lines":[
                {"startTimeMs":"1000","words":"Hola mundo","syllables":[{"startTimeMs":"1000","numChars":5},{"startTimeMs":1400,"numChars":"5"}],"endTimeMs":"2000"},
                {"startTimeMs":"2500","words":"adiós","syllables":[],"endTimeMs":"0"}]}}"#,
        )
        .unwrap();
        let l = Lyrics::from_json("t", &v).unwrap();
        assert!(l.synced());
        assert_eq!(l.lines[0].syllables, vec![Syllable { start_ms: 1000, chars: 5 }, Syllable { start_ms: 1400, chars: 5 }]);
        assert_eq!(l.lines[0].end_ms, Some(2000));
        assert!(l.lines[1].syllables.is_empty());
        assert_eq!(l.lines[1].end_ms, None);
        assert_eq!(l.current_line(1500), Some(0));
    }

    /// LRC con marcas por palabra (`<mm:ss.xx>`): el texto queda limpio y cada palabra con su
    /// tiempo; sin marcas, igual que siempre.
    #[test]
    fn lrc_con_tiempos_por_palabra() {
        let l = Lyrics::from_lrc("t", "[00:01.00] <00:01.00> Hola <00:01.50> mundo\n[00:03.00]Sin marcas", "LRCLIB");
        assert_eq!(l.lines[0].words, "Hola mundo");
        assert_eq!(l.lines[0].syllables, vec![Syllable { start_ms: 1000, chars: 5 }, Syllable { start_ms: 1500, chars: 5 }]);
        assert_eq!(l.lines[1].words, "Sin marcas");
        assert!(l.lines[1].syllables.is_empty());
        assert_eq!(l.lines[1].start_ms, 3000);
    }
}
