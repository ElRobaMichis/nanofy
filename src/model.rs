//! Modelos de datos de la Web API de Spotify (deserialización tolerante) y tipos de la interfaz.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

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
}

impl Playlist {
    pub fn cover(&self, want: u32) -> Option<&str> {
        self.images.as_deref().and_then(|i| pick_image(i, want))
    }
    pub fn owner_name(&self) -> &str {
        self.owner.display_name.as_deref().unwrap_or("")
    }
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
    #[serde(default)]
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
    #[serde(default)]
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LyricLine {
    pub start_ms: u32,
    pub words: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lyrics {
    pub track_id: String,
    /// "LINE_SYNCED" o "UNSYNCED"
    pub sync_type: String,
    pub lines: Vec<LyricLine>,
    pub provider: String,
}

impl Lyrics {
    pub fn synced(&self) -> bool {
        self.sync_type == "LINE_SYNCED"
    }

    pub fn from_json(track_id: &str, v: &serde_json::Value) -> Option<Self> {
        let l = v.get("lyrics")?;
        let lines = l
            .get("lines")?
            .as_array()?
            .iter()
            .map(|line| LyricLine {
                start_ms: line
                    .get("startTimeMs")
                    .and_then(|s| s.as_str().and_then(|s| s.parse().ok()).or_else(|| s.as_u64().map(|n| n as u32)))
                    .unwrap_or(0),
                words: line
                    .get("words")
                    .and_then(|w| w.as_str())
                    .unwrap_or("")
                    .to_string(),
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
            let mut parts = stamp.split(':');
            let (Some(m), Some(s)) = (parts.next(), parts.next()) else { continue };
            let (Ok(m), Ok(s)) = (m.parse::<u32>(), s.parse::<f32>()) else { continue };
            lines.push(LyricLine {
                start_ms: m * 60_000 + (s * 1000.0) as u32,
                words: text.trim().to_string(),
            });
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
