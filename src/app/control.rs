//! Operaciones del modo de control (`--control <puerto>`, ver `src/control.rs`).
//!
//! Cada operación llama a los mismos métodos que los botones, menús y atajos de la interfaz, y
//! `state` devuelve una fotografía del estado real de la app para que el runner de pruebas
//! (carpeta `qa/`) pueda esperar y comprobar resultados. Todo se ejecuta en el hilo de la interfaz.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{Action, ActiveTab, App, Auth, Page, PlayState, PlayTarget, PlaylistEditor, Repeat, Shortcut, SideTab};
use crate::api::Req;
use crate::backend::Cmd;
use crate::config::vol_pct_to_raw;
use crate::update::InstallProgress;

fn s<'a>(cmd: &'a Value, key: &str) -> Option<&'a str> {
    cmd.get(key).and_then(|v| v.as_str())
}
fn b(cmd: &Value, key: &str, default: bool) -> bool {
    cmd.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}
fn n(cmd: &Value, key: &str) -> Option<i64> {
    cmd.get(key).and_then(|v| v.as_i64())
}
fn strings(cmd: &Value, key: &str) -> Vec<String> {
    cmd.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}
fn ok() -> Value {
    json!({"ok": true})
}
fn err(msg: impl Into<String>) -> Value {
    json!({"ok": false, "error": msg.into()})
}

/// `home`, `liked`, `playlist:<id>`, `album:<id>`… (las mismas claves que `--page`).
pub fn parse_page(spec: &str) -> Option<Page> {
    Some(match spec {
        "home" => Page::Home,
        "library" => Page::Library,
        "search" => Page::Search,
        "liked" => Page::Liked,
        "albums" => Page::Albums,
        "artists" => Page::Artists,
        "settings" => Page::Settings,
        "history" => Page::History,
        "saves" => Page::Saves,
        "shows" => Page::Shows,
        "audiobooks" => Page::Audiobooks,
        "folders" => Page::Folders,
        s if s.starts_with("playlist:") => Page::Playlist(s[9..].to_string()),
        s if s.starts_with("album:") => Page::Album(s[6..].to_string()),
        s if s.starts_with("artist:") => Page::Artist(s[7..].to_string()),
        s if s.starts_with("user:") => Page::User(s[5..].to_string()),
        s if s.starts_with("show:") => Page::Show(s[5..].to_string()),
        _ => return None,
    })
}

fn page_spec(p: &Page) -> String {
    match p {
        Page::Home => "home".into(),
        Page::Library => "library".into(),
        Page::Search => "search".into(),
        Page::Liked => "liked".into(),
        Page::Albums => "albums".into(),
        Page::Artists => "artists".into(),
        Page::Settings => "settings".into(),
        Page::History => "history".into(),
        Page::Saves => "saves".into(),
        Page::Shows => "shows".into(),
        Page::Audiobooks => "audiobooks".into(),
        Page::Folders => "folders".into(),
        Page::Playlist(id) => format!("playlist:{id}"),
        Page::Album(id) => format!("album:{id}"),
        Page::Artist(id) => format!("artist:{id}"),
        Page::User(id) => format!("user:{id}"),
        Page::Show(id) => format!("show:{id}"),
    }
}

impl Shortcut {
    fn from_name(name: &str) -> Option<Shortcut> {
        Some(match name {
            "quit" => Shortcut::Quit,
            "search" => Shortcut::Search,
            "next" => Shortcut::Next,
            "prev" => Shortcut::Prev,
            "vol_up" => Shortcut::VolUp,
            "vol_down" => Shortcut::VolDown,
            "seek_fwd" => Shortcut::SeekFwd,
            "seek_back" => Shortcut::SeekBack,
            "back" => Shortcut::Back,
            "forward" => Shortcut::Forward,
            "home" => Shortcut::Home,
            "liked" => Shortcut::Liked,
            "sidebar" => Shortcut::Sidebar,
            "settings" => Shortcut::Settings,
            "help" => Shortcut::Help,
            "play_pause" => Shortcut::PlayPause,
            "shuffle" => Shortcut::Shuffle,
            "repeat" => Shortcut::Repeat,
            "mute" => Shortcut::Mute,
            "queue" => Shortcut::Queue,
            "lyrics" => Shortcut::Lyrics,
            "new_playlist" => Shortcut::NewPlaylist,
            "jam" => Shortcut::Jam,
            "close_tab" => Shortcut::CloseTab,
            "escape" => Shortcut::Escape,
            _ => return None,
        })
    }
}

impl App {
    fn auth_signed_in_for_control(&self) -> bool {
        matches!(self.auth, Auth::LoggedIn { .. } | Auth::Connecting { .. })
    }

    pub fn control_start(&self, port: u16) {
        crate::control::start(port, self.ui_tx.clone());
    }

    /// Ejecuta una operación y devuelve la respuesta JSON.
    pub fn control_exec(&mut self, ctx: &egui::Context, cmd: &Value) -> Value {
        let op = s(cmd, "op").unwrap_or("");
        log::debug!("[control] {cmd}");
        match op {
            "state" => json!({"ok": true, "state": self.control_state()}),
            "ping" => json!({"ok": true, "version": crate::update::current_version()}),

            // ------------------------------------------------------------ navegación
            "go" => match s(cmd, "page").and_then(parse_page) {
                Some(p) => {
                    if p == Page::Settings {
                        self.draft = self.settings.clone();
                    }
                    self.go(p);
                    ok()
                }
                None => err("página desconocida"),
            },
            "open_tab" => match s(cmd, "page").and_then(parse_page) {
                Some(p) => {
                    let i = self.open_tab(p);
                    json!({"ok": true, "index": i})
                }
                None => err("página desconocida"),
            },
            "close_tab" => match n(cmd, "index") {
                Some(i) if (i as usize) < self.tabs.len() => {
                    self.close_tab(i as usize);
                    ok()
                }
                Some(_) => err("índice de pestaña fuera de rango"),
                None => {
                    if let ActiveTab::Tab(i) = self.active {
                        self.close_tab(i);
                        ok()
                    } else {
                        err("la pestaña activa no se puede cerrar")
                    }
                }
            },
            "activate_tab" => match s(cmd, "tab") {
                Some("home") => {
                    self.active = ActiveTab::Home;
                    ok()
                }
                Some("search") => {
                    self.active = ActiveTab::Search;
                    ok()
                }
                _ => match n(cmd, "index") {
                    Some(i) if (i as usize) < self.tabs.len() => {
                        self.actions.push(Action::ActivateTab(i as usize));
                        ok()
                    }
                    _ => err("índice de pestaña fuera de rango"),
                },
            },
            "back" => {
                self.back();
                ok()
            }
            "forward" => {
                self.forward();
                ok()
            }
            "shortcut" => match s(cmd, "name").and_then(Shortcut::from_name) {
                Some(sc) => {
                    self.run_shortcut(sc, ctx);
                    ok()
                }
                None => err("atajo desconocido"),
            },

            // ----------------------------------------------------------- reproducción
            "play" => {
                let shuffle = b(cmd, "shuffle", self.player.shuffle);
                let index = n(cmd, "index").map(|i| i as u32);
                let target = if let Some(uri) = s(cmd, "context_uri") {
                    PlayTarget::Context {
                        uri: uri.to_string(),
                        track_uri: s(cmd, "track_uri").map(str::to_string),
                        index,
                        shuffle,
                    }
                } else {
                    let uris = strings(cmd, "uris");
                    if uris.is_empty() {
                        return err("hace falta context_uri o uris");
                    }
                    PlayTarget::Tracks { uris, index, shuffle }
                };
                self.play(target);
                ok()
            }
            "play_pause" => {
                self.play_pause();
                ok()
            }
            "next" => {
                self.next();
                ok()
            }
            "prev" => {
                self.prev();
                ok()
            }
            "seek" => match n(cmd, "ms") {
                Some(ms) => {
                    self.seek(ms.max(0) as u32);
                    ok()
                }
                None => err("falta ms"),
            },
            "seek_by" => match n(cmd, "delta_ms") {
                Some(d) => {
                    self.seek_by(d);
                    ok()
                }
                None => err("falta delta_ms"),
            },
            "volume" => match n(cmd, "pct") {
                Some(p) => {
                    self.set_volume(vol_pct_to_raw(p as f32));
                    ok()
                }
                None => err("falta pct"),
            },
            "volume_by" => match n(cmd, "delta") {
                Some(d) => {
                    self.volume_by(d as i32);
                    ok()
                }
                None => err("falta delta"),
            },
            "mute" => {
                self.toggle_mute();
                ok()
            }
            "shuffle" => {
                match cmd.get("on").and_then(|v| v.as_bool()) {
                    Some(on) if on == self.player.shuffle => {}
                    _ => self.toggle_shuffle(),
                }
                ok()
            }
            "repeat" => {
                match s(cmd, "mode") {
                    Some(mode) => {
                        let want = match mode {
                            "off" => Repeat::Off,
                            "context" => Repeat::Context,
                            "track" => Repeat::Track,
                            _ => return err("modo de repetición desconocido"),
                        };
                        let mut guard = 0;
                        while self.player.repeat != want && guard < 3 {
                            self.cycle_repeat();
                            guard += 1;
                        }
                    }
                    None => self.cycle_repeat(),
                }
                ok()
            }
            "like" => match s(cmd, "id") {
                Some(id) => {
                    self.like(id.to_string(), b(cmd, "on", true));
                    ok()
                }
                None => err("falta id"),
            },
            "save_album" => match s(cmd, "id") {
                Some(id) => {
                    self.actions.push(Action::SaveAlbum(id.to_string(), b(cmd, "on", true)));
                    ok()
                }
                None => err("falta id"),
            },
            "follow" => match (s(cmd, "kind"), s(cmd, "id")) {
                (Some(kind), Some(id)) => {
                    let kind: &'static str = match kind {
                        "artist" => "artist",
                        "user" => "user",
                        _ => return err("kind debe ser artist o user"),
                    };
                    self.actions.push(Action::Follow { kind, id: id.to_string(), on: b(cmd, "on", true) });
                    ok()
                }
                _ => err("faltan kind e id"),
            },
            "follow_playlist" => match s(cmd, "id") {
                Some(id) => {
                    self.actions.push(Action::FollowPlaylist(id.to_string(), b(cmd, "on", true)));
                    ok()
                }
                None => err("falta id"),
            },
            "follow_show" => match s(cmd, "id") {
                Some(id) => {
                    self.actions.push(Action::FollowShow(id.to_string(), b(cmd, "on", true)));
                    ok()
                }
                None => err("falta id"),
            },
            "save_episode" => match s(cmd, "id") {
                Some(id) => {
                    self.actions.push(Action::SaveEpisode(id.to_string(), b(cmd, "on", true)));
                    ok()
                }
                None => err("falta id"),
            },
            "queue_add" => match s(cmd, "uri") {
                Some(uri) => {
                    self.actions.push(Action::AddToQueue(uri.to_string()));
                    ok()
                }
                None => err("falta uri"),
            },
            "queue_remove" => match s(cmd, "uri") {
                Some(uri) => {
                    self.queue_remove(uri);
                    ok()
                }
                None => err("falta uri"),
            },
            "queue_clear" => {
                self.queue_clear();
                ok()
            }
            "queue_play" => {
                // Doble clic en una fila de la cola (misma ruta que la interfaz).
                let Some(i) = n(cmd, "index").map(|i| i as usize) else { return err("falta index") };
                let rows: Vec<crate::model::Track> = self.queue.as_ref().map(|q| q.queue.clone()).unwrap_or_default();
                let Some(t) = rows.get(i).cloned() else { return err("índice fuera de la cola") };
                self.play_from_queue(&t.uri, &rows, i);
                ok()
            }
            "volume_wheel" => match cmd.get("points").and_then(|v| v.as_f64()) {
                // Rueda del ratón sobre el volumen: 50 puntos por muesca (25 = 1 %).
                Some(p) => {
                    self.volume_wheel(p as f32);
                    ok()
                }
                None => err("falta points"),
            },
            "minimize" => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(b(cmd, "on", true)));
                ok()
            }
            "queue_refresh" => {
                self.api.send(Req::Queue);
                self.queue_at = Instant::now();
                ok()
            }
            "side" => {
                match s(cmd, "tab") {
                    Some("queue") => {
                        if self.side != Some(SideTab::Queue) {
                            self.toggle_side(SideTab::Queue)
                        }
                    }
                    Some("lyrics") => {
                        if self.side != Some(SideTab::Lyrics) {
                            self.toggle_side(SideTab::Lyrics)
                        }
                    }
                    Some("none") | None => self.side = None,
                    _ => return err("tab debe ser queue, lyrics o none"),
                }
                ok()
            }
            "lyrics" => {
                self.ensure_lyrics();
                ok()
            }
            "radio" => match s(cmd, "track_id") {
                Some(id) => {
                    self.actions.push(Action::OpenRadio(id.to_string()));
                    ok()
                }
                None => err("falta track_id"),
            },
            "devices" => {
                self.api.send(Req::Devices);
                ok()
            }
            "select_device" => {
                let me = self.device_id.clone();
                let target = s(cmd, "name").map(str::to_string);
                let want_self = target.as_deref() == Some("self") || b(cmd, "self", false);
                let found = self.devices.iter().find(|d| {
                    if want_self {
                        d.id.as_deref() == Some(me.as_str())
                    } else {
                        Some(d.name.as_str()) == target.as_deref() || d.id.as_deref() == s(cmd, "id")
                    }
                });
                match found.cloned() {
                    Some(d) => {
                        let is_self = d.id.as_deref() == Some(me.as_str());
                        self.select_device(d, is_self);
                        ok()
                    }
                    None if want_self => {
                        // Este equipo puede no figurar en la lista todavía: se pide igualmente.
                        self.select_device(
                            crate::model::Device { id: Some(me), name: self.settings.device_name.clone(), is_active: false, kind: "Computer".into(), volume_percent: None },
                            true,
                        );
                        ok()
                    }
                    None => err("dispositivo no encontrado"),
                }
            }
            "sleep" => {
                match cmd.get("minutes").and_then(|v| v.as_u64()) {
                    Some(m) if m > 0 => {
                        self.sleep_at = Some(Instant::now() + Duration::from_secs(m * 60));
                        self.sleep_end_of_track = false;
                    }
                    _ => match s(cmd, "mode") {
                        Some("end") => {
                            self.sleep_end_of_track = true;
                            self.sleep_at = None;
                        }
                        _ => {
                            self.sleep_at = None;
                            self.sleep_end_of_track = false;
                        }
                    },
                }
                ok()
            }
            "hide_track" => match s(cmd, "id") {
                Some(id) => {
                    self.toggle_hidden(id);
                    ok()
                }
                None => err("falta id"),
            },
            "download" => {
                let ids = strings(cmd, "ids");
                if ids.is_empty() {
                    return err("faltan ids");
                }
                self.download(ids);
                ok()
            }

            // ------------------------------------------------------- búsqueda y enlaces
            "search" => match s(cmd, "q") {
                Some(q) => {
                    self.search_query = q.to_string();
                    self.run_search();
                    ok()
                }
                None => err("falta q"),
            },
            "search_filter" => match n(cmd, "n") {
                Some(f) => {
                    self.search_filter = f as u8;
                    ok()
                }
                None => err("falta n"),
            },
            "open_link" => match s(cmd, "link") {
                Some(l) => {
                    self.actions.push(Action::OpenLink(l.to_string()));
                    ok()
                }
                None => err("falta link"),
            },

            // -------------------------------------------------------------- playlists
            "playlist_create" => {
                let Some(user_id) = self.my_id().map(str::to_string) else {
                    return err("perfil no cargado todavía");
                };
                let name = s(cmd, "name").unwrap_or("Nueva playlist").trim().to_string();
                if name.is_empty() {
                    return err("el nombre no puede estar vacío (la interfaz desactiva Crear)");
                }
                let ed = PlaylistEditor {
                    id: None,
                    name: name.clone(),
                    description: s(cmd, "description").unwrap_or("").trim().to_string(),
                    public: b(cmd, "public", true),
                    collaborative: b(cmd, "collaborative", false),
                    image_path: s(cmd, "image").map(std::path::PathBuf::from),
                    busy: true,
                };
                self.api.send(Req::CreatePlaylist {
                    user_id,
                    name,
                    description: ed.description.clone(),
                    public: ed.public,
                    collaborative: ed.collaborative,
                });
                self.editor = Some(ed);
                ok()
            }
            "playlist_update" => {
                let Some(id) = s(cmd, "id") else { return err("falta id") };
                let current = self.playlists.iter().find(|p| p.id == id).cloned().or_else(|| self.playlist_meta.get(id).cloned());
                let name = s(cmd, "name").map(str::to_string).or_else(|| current.as_ref().map(|p| p.name.clone())).unwrap_or_default();
                if name.trim().is_empty() {
                    return err("el nombre no puede estar vacío (la interfaz desactiva Guardar)");
                }
                let description = s(cmd, "description")
                    .map(str::to_string)
                    .or_else(|| current.as_ref().and_then(|p| p.description.clone()))
                    .unwrap_or_default();
                let public = cmd.get("public").and_then(|v| v.as_bool()).or_else(|| current.as_ref().and_then(|p| p.public)).unwrap_or(true);
                let collaborative = cmd.get("collaborative").and_then(|v| v.as_bool()).or_else(|| current.as_ref().and_then(|p| p.collaborative)).unwrap_or(false);
                self.editor = Some(PlaylistEditor {
                    id: Some(id.to_string()),
                    name: name.trim().to_string(),
                    description: description.trim().to_string(),
                    public,
                    collaborative,
                    image_path: None,
                    busy: true,
                });
                self.api.send(Req::UpdatePlaylist {
                    id: id.to_string(),
                    name: name.trim().to_string(),
                    description: description.trim().to_string(),
                    public,
                    collaborative,
                });
                ok()
            }
            "playlist_add" => match s(cmd, "id") {
                Some(id) => {
                    let uris = strings(cmd, "uris");
                    if uris.is_empty() {
                        return err("faltan uris");
                    }
                    for uri in uris {
                        self.actions.push(Action::AddToPlaylist { playlist_id: id.to_string(), uri });
                    }
                    ok()
                }
                None => err("falta id"),
            },
            "playlist_remove" => match s(cmd, "id") {
                Some(id) => {
                    let uris = strings(cmd, "uris");
                    if uris.is_empty() {
                        return err("faltan uris");
                    }
                    for uri in uris {
                        self.actions.push(Action::RemoveFromPlaylist { playlist_id: id.to_string(), uri });
                    }
                    ok()
                }
                None => err("falta id"),
            },
            "playlist_image" => match (s(cmd, "id"), s(cmd, "path")) {
                (Some(id), Some(path)) => {
                    self.api.send(Req::SetPlaylistImage { id: id.to_string(), path: std::path::PathBuf::from(path) });
                    self.status("Subiendo la imagen de la playlist…");
                    ok()
                }
                _ => err("faltan id y path"),
            },
            "pin" => match s(cmd, "id") {
                Some(id) => {
                    self.actions.push(Action::Pin(id.to_string(), b(cmd, "on", true)));
                    ok()
                }
                None => err("falta id"),
            },
            "editor_open" => {
                let p = s(cmd, "id").and_then(|id| self.playlists.iter().find(|p| p.id == id).cloned());
                if s(cmd, "id").is_some() && p.is_none() {
                    return err("playlist no cargada");
                }
                self.actions.push(Action::OpenEditor(p));
                ok()
            }
            "editor_cancel" => {
                self.editor = None;
                ok()
            }
            "add_dialog_open" => {
                let uris = strings(cmd, "uris");
                if uris.is_empty() {
                    return err("faltan uris");
                }
                self.open_add_dialog(uris);
                ok()
            }
            "add_dialog_close" => {
                self.add_dialog = None;
                ok()
            }
            "folder_create" => match s(cmd, "name") {
                Some(name) if !name.trim().is_empty() => {
                    self.api.send(Req::FolderCreate { name: name.trim().to_string(), playlists: strings(cmd, "playlists") });
                    ok()
                }
                Some(_) => err("el nombre no puede estar vacío (la interfaz desactiva Crear)"),
                None => err("falta name"),
            },
            "folder_rename" => match (s(cmd, "id"), s(cmd, "name")) {
                (Some(_), Some(name)) if name.trim().is_empty() => err("el nombre no puede estar vacío"),
                (Some(id), Some(name)) => {
                    self.api.send(Req::FolderRename { id: id.to_string(), name: name.to_string() });
                    ok()
                }
                _ => err("faltan id y name"),
            },
            "folder_delete" => match s(cmd, "id") {
                Some(id) => {
                    self.api.send(Req::FolderDelete(id.to_string()));
                    ok()
                }
                None => err("falta id"),
            },
            "folder_move" => match s(cmd, "playlist") {
                Some(pl) => {
                    self.api.send(Req::FolderMove { playlist: pl.to_string(), folder: s(cmd, "folder").map(str::to_string) });
                    ok()
                }
                None => err("falta playlist"),
            },
            "invite_link" => match s(cmd, "playlist") {
                Some(pl) => {
                    self.request_invite(pl);
                    ok()
                }
                None => err("falta playlist"),
            },
            "members" => match s(cmd, "playlist") {
                Some(pl) => {
                    self.invalidate(&format!("members:{pl}"));
                    self.request_once(&format!("members:{pl}"), Req::Members(pl.to_string()));
                    ok()
                }
                None => err("falta playlist"),
            },
            "set_member" => match (s(cmd, "playlist"), s(cmd, "user")) {
                (Some(pl), Some(u)) => {
                    self.api.send(Req::SetMember { playlist: pl.to_string(), user: u.to_string(), contributor: b(cmd, "contributor", true) });
                    ok()
                }
                _ => err("faltan playlist y user"),
            },
            "set_base" => match s(cmd, "playlist") {
                Some(pl) => {
                    self.api.send(Req::SetBase { playlist: pl.to_string(), contributor: b(cmd, "contributor", true) });
                    ok()
                }
                None => err("falta playlist"),
            },

            // ------------------------------------------------------------------ Jam
            "jam_open" => {
                self.jam_open = b(cmd, "on", true);
                ok()
            }
            "jam_join" => match s(cmd, "token") {
                Some(t) => {
                    self.jam_join(t);
                    ok()
                }
                None => err("falta token"),
            },
            "jam_leave" => match self.jam.as_ref().map(|j| j.session_id.clone()) {
                Some(id) => {
                    self.jam_busy = true;
                    self.api.send(Req::JamLeave(id));
                    ok()
                }
                None => err("no hay Jam activa"),
            },
            "jam_end" => match self.jam.as_ref().map(|j| j.session_id.clone()) {
                Some(id) => {
                    self.jam_busy = true;
                    self.api.send(Req::JamEnd(id));
                    ok()
                }
                None => err("no hay Jam activa"),
            },
            "jam_current" => {
                self.api.send(Req::JamCurrent);
                ok()
            }

            // -------------------------------------------------------- ajustes y vista
            "settings" => {
                let Some(patch) = cmd.get("patch").and_then(|v| v.as_object()) else {
                    return err("falta patch");
                };
                let mut v = match serde_json::to_value(&self.draft) {
                    Ok(v) => v,
                    Err(e) => return err(e.to_string()),
                };
                if let Some(obj) = v.as_object_mut() {
                    for (k, val) in patch {
                        if !obj.contains_key(k) {
                            return err(format!("ajuste desconocido: {k}"));
                        }
                        obj.insert(k.clone(), val.clone());
                    }
                }
                match serde_json::from_value::<crate::config::Settings>(v) {
                    Ok(d) => {
                        self.draft = d;
                        self.save_settings(ctx);
                        ok()
                    }
                    Err(e) => err(format!("valor no válido: {e}")),
                }
            }
            "settings_discard" => {
                self.draft = self.settings.clone();
                ok()
            }
            "theme" => {
                let t = match s(cmd, "name") {
                    Some("system") => crate::config::Theme::System,
                    Some("dark") => crate::config::Theme::Dark,
                    Some("light") => crate::config::Theme::Light,
                    _ => return err("name debe ser system, dark o light"),
                };
                self.draft = self.settings.clone();
                self.draft.theme = t;
                self.save_settings(ctx);
                ok()
            }
            "sidebar" => {
                let on = b(cmd, "on", !self.settings.sidebar_visible);
                if on != self.settings.sidebar_visible {
                    self.run_shortcut(Shortcut::Sidebar, ctx);
                }
                ok()
            }
            "miniplayer" => {
                let want = b(cmd, "on", !self.miniplayer);
                if want != self.miniplayer {
                    self.toggle_miniplayer(ctx);
                }
                ok()
            }
            "fullscreen" => {
                let want = b(cmd, "on", !self.fullscreen);
                if want != self.fullscreen {
                    self.toggle_fullscreen(ctx);
                }
                ok()
            }
            "library_grid" => {
                self.library_grid = b(cmd, "on", !self.library_grid);
                self.settings.library_grid = self.library_grid;
                ok()
            }
            "library_sort_name" => {
                self.library_sort_name = b(cmd, "on", !self.library_sort_name);
                ok()
            }
            "library_filter" => {
                self.library_filter = s(cmd, "q").unwrap_or("").to_string();
                ok()
            }
            "home_filter" => match n(cmd, "n") {
                Some(f) => {
                    self.home_filter = f as u8;
                    ok()
                }
                None => err("falta n"),
            },
            "artist_tab" => match n(cmd, "n") {
                Some(t) => {
                    self.artist_tab = t as u8;
                    ok()
                }
                None => err("falta n"),
            },
            "artist_grid" => {
                self.artist_grid = b(cmd, "on", !self.artist_grid);
                ok()
            }
            "home_pin" | "home_hide" => match s(cmd, "id") {
                Some(id) => {
                    let on = b(cmd, "on", true);
                    let list = if op == "home_pin" { &mut self.settings.home_pinned } else { &mut self.settings.home_hidden };
                    list.retain(|x| x != id);
                    if on {
                        list.push(id.to_string());
                    }
                    self.settings.save(&self.paths);
                    self.home_cache = None;
                    ok()
                }
                None => err("falta id"),
            },
            "stall" => {
                // Simula «lleva demasiado cargando»: fuerza la reconexión del backend.
                self.backend.send(Cmd::Stalled);
                ok()
            }
            "check_updates" => {
                self.check_updates(true);
                ok()
            }
            "update_install" => {
                self.install_update();
                ok()
            }
            "update_skip" => {
                self.skip_update();
                ok()
            }
            "update_banner" => {
                self.update_banner = b(cmd, "on", true) && self.update.is_some();
                ok()
            }
            "frame_reset" => {
                self.frame_hist.clear();
                self.frame_phases = [0.0; 4];
                ok()
            }
            "status_clear" => {
                self.status = None;
                ok()
            }
            "login" => {
                self.login();
                ok()
            }
            "logout" => {
                self.backend.send(Cmd::Logout);
                self.go(Page::Home);
                ok()
            }
            "request" => self.control_request(cmd),
            "invalidate" => match s(cmd, "key") {
                Some(k) => {
                    self.invalidate(k);
                    ok()
                }
                None => err("falta key"),
            },
            "reload_library" => {
                self.reload_library();
                ok()
            }
            "quit" => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ok()
            }
            _ => err(format!("operación desconocida: {op}")),
        }
    }

    /// Peticiones de lectura sueltas a la API (para precargar o forzar una recarga).
    fn control_request(&mut self, cmd: &Value) -> Value {
        let name = s(cmd, "name").unwrap_or("");
        let id = s(cmd, "id").unwrap_or("").to_string();
        let need_id = |req: Req| -> Value {
            if id.is_empty() {
                return err("falta id");
            }
            self.api.send(req);
            ok()
        };
        match name {
            "me" => {
                self.api.send(Req::Me);
                ok()
            }
            "playlists" => {
                self.api.send(Req::Playlists);
                ok()
            }
            "liked" => {
                self.api.send(Req::Liked);
                ok()
            }
            "liked_recent" => {
                self.api.send(Req::LikedRecent);
                ok()
            }
            "saved_albums" => {
                self.api.send(Req::SavedAlbums);
                ok()
            }
            "followed_artists" => {
                self.api.send(Req::FollowedArtists);
                ok()
            }
            "recent" => {
                self.api.send(Req::Recent);
                ok()
            }
            "devices" => {
                self.api.send(Req::Devices);
                ok()
            }
            "player_state" => {
                self.api.send(Req::PlayerState);
                ok()
            }
            "queue" => {
                self.api.send(Req::Queue);
                ok()
            }
            "home_feed" => {
                self.api.send(Req::HomeFeed);
                ok()
            }
            "rootlist" => {
                self.api.send(Req::Rootlist);
                ok()
            }
            "saved_shows" => {
                self.api.send(Req::SavedShows);
                ok()
            }
            "saved_episodes" => {
                self.api.send(Req::SavedEpisodes);
                ok()
            }
            "saved_audiobooks" => {
                self.api.send(Req::SavedAudiobooks);
                ok()
            }
            "last_playback" => {
                self.api.send(Req::LastPlayback);
                ok()
            }
            "playlist_tracks" => need_id(Req::PlaylistTracks(id.clone())),
            "playlist_meta" => need_id(Req::PlaylistMeta(id.clone())),
            "album" => need_id(Req::Album(id.clone())),
            "artist" => need_id(Req::Artist(id.clone())),
            "artist_top" => need_id(Req::ArtistTop(id.clone())),
            "artist_albums" => need_id(Req::ArtistAlbums(id.clone())),
            "artist_view" => need_id(Req::ArtistView(id.clone())),
            "track_info" => need_id(Req::TrackInfo(id.clone())),
            "user" => need_id(Req::User(id.clone())),
            "user_playlists" => need_id(Req::UserPlaylists(id.clone())),
            "show" => need_id(Req::Show(id.clone())),
            "probe" => need_id(Req::Probe(id.clone())),
            "follow_contains" => {
                let kind: &'static str = match s(cmd, "kind") {
                    Some("artist") => "artist",
                    Some("user") => "user",
                    _ => return err("kind debe ser artist o user"),
                };
                let ids = strings(cmd, "ids");
                if ids.is_empty() {
                    return err("faltan ids");
                }
                self.api.send(Req::FollowContains { kind, ids });
                ok()
            }
            _ => err(format!("petición desconocida: {name}")),
        }
    }

    /// Fotografía del estado de la app (solo datos; nada de egui).
    pub fn control_state(&self) -> Value {
        let auth = match &self.auth {
            Auth::LoggedOut => json!({"state": "logged_out"}),
            Auth::LoggingIn => json!({"state": "logging_in"}),
            Auth::Connecting { username } => json!({"state": "connecting", "username": username}),
            Auth::LoggedIn { username } => json!({"state": "logged_in", "username": username}),
        };
        let now = self.player.now.as_ref().map(|n| {
            json!({
                "uri": n.uri, "id": n.id, "name": n.name, "album": n.album, "album_id": n.album_id,
                "artists": n.artists.iter().map(|(name, id)| json!({"name": name, "id": id})).collect::<Vec<_>>(),
                "duration_ms": n.duration_ms, "cover": n.cover_url,
            })
        });
        let player = json!({
            "state": match self.player.state { PlayState::Stopped => "stopped", PlayState::Loading => "loading", PlayState::Playing => "playing", PlayState::Paused => "paused" },
            "now": now,
            "position_ms": self.player.position(),
            "volume_raw": self.player.volume,
            "volume_pct": crate::config::vol_raw_to_pct(self.player.volume).round() as u32,
            "shuffle": self.player.shuffle,
            "repeat": match self.player.repeat { Repeat::Off => "off", Repeat::Context => "context", Repeat::Track => "track" },
            "remote": self.player.remote.as_ref().map(|d| json!({"name": d.name, "id": d.id})),
            "liked": self.player.liked,
            "muted": self.player.volume == 0,
            "last_play": self.last_play.as_ref().map(|t| match t {
                PlayTarget::Context { uri, track_uri, index, shuffle } => json!({"context_uri": uri, "track_uri": track_uri, "index": index, "shuffle": shuffle}),
                PlayTarget::Tracks { uris, index, shuffle } => json!({"uris": uris, "index": index, "shuffle": shuffle}),
            }),
        });
        let tracks_json = |list: &[crate::model::Track]| -> Vec<Value> {
            list.iter().map(|t| json!({"uri": t.uri, "id": t.id, "name": t.name, "artist": t.artists.first().map(|a| a.name.clone()), "duration_ms": t.duration_ms})).collect()
        };
        let lists: serde_json::Map<String, Value> = self
            .lists
            .iter()
            .map(|(k, l)| (k.clone(), json!({"count": l.tracks.len(), "total": l.total, "loading": l.loading, "uris": l.tracks.iter().map(|t| t.uri.clone()).collect::<Vec<_>>()})))
            .collect();
        let playlists: Vec<Value> = self
            .playlists
            .iter()
            .map(|p| {
                json!({
                    "id": p.id, "name": p.name, "uri": p.uri, "public": p.public, "collaborative": p.collaborative,
                    "description": p.description, "owner": p.owner.id, "tracks_total": p.tracks.as_ref().map(|t| t.total),
                    "mine": self.is_mine(p), "pinned": self.settings.pinned.contains(&p.id),
                })
            })
            .collect();
        let search = self.search_result.as_ref().map(|r| {
            json!({
                "tracks": r.tracks.as_ref().map(|p| p.items.len()).unwrap_or(0),
                "albums": r.albums.as_ref().map(|p| p.items.len()).unwrap_or(0),
                "artists": r.artists.as_ref().map(|p| p.items.len()).unwrap_or(0),
                "playlists": r.playlists.as_ref().map(|p| p.items.len()).unwrap_or(0),
                "shows": r.shows.as_ref().map(|p| p.items.len()).unwrap_or(0),
                "episodes": r.episodes.as_ref().map(|p| p.items.len()).unwrap_or(0),
                "audiobooks": r.audiobooks.as_ref().map(|p| p.items.len()).unwrap_or(0),
                "first_track": r.tracks.as_ref().and_then(|p| p.items.first()).map(|t| json!({"uri": t.uri, "id": t.id, "name": t.name})),
                "first_album": r.albums.as_ref().and_then(|p| p.items.iter().flatten().next()).map(|a| json!({"id": a.id, "name": a.name, "uri": a.uri})),
                "first_artist": r.artists.as_ref().and_then(|p| p.items.iter().flatten().next()).map(|a| json!({"id": a.id, "name": a.name})),
                "first_playlist": r.playlists.as_ref().and_then(|p| p.items.iter().flatten().next()).map(|p| json!({"id": p.id, "name": p.name, "owner": p.owner.id})),
                "first_show": r.shows.as_ref().and_then(|p| p.items.iter().flatten().next()).map(|s| json!({"id": s.id, "name": s.name})),
                "first_episode": r.episodes.as_ref().and_then(|p| p.items.iter().flatten().next()).map(|e| json!({"id": e.id, "name": e.name, "uri": e.uri})),
                "first_audiobook": r.audiobooks.as_ref().and_then(|p| p.items.iter().flatten().next()).map(|a| json!({"id": a.id, "name": a.name})),
            })
        });
        let queue = self.queue.as_ref().map(|q| json!({"current": q.currently_playing.as_ref().map(|t| t.uri.clone()), "items": tracks_json(&q.queue)}));
        let lyrics = self.lyrics.as_ref().map(|l| json!({"track_id": l.track_id, "sync": l.sync_type, "lines": l.lines.len(), "provider": l.provider, "first": l.lines.first().map(|x| x.words.clone())}));
        let artists: serde_json::Map<String, Value> = self
            .artists
            .iter()
            .map(|(k, a)| (k.clone(), json!({"loaded": a.artist.is_some(), "name": a.artist.as_ref().map(|x| x.name.clone())})))
            .collect();
        let albums: serde_json::Map<String, Value> = self
            .albums
            .iter()
            .map(|(k, a)| (k.clone(), json!({"name": a.name, "tracks": a.tracks.as_ref().map(|t| t.items.len()), "artist": a.artists.first().map(|x| x.name.clone()), "uris": a.tracks.as_ref().map(|t| t.items.iter().map(|x| x.uri.clone()).collect::<Vec<_>>()).unwrap_or_default()})))
            .collect();
        let users: serde_json::Map<String, Value> = self
            .users
            .iter()
            .map(|(k, u)| (k.clone(), json!({"name": u.display_name, "followers": u.followers.as_ref().and_then(|f| f.total)})))
            .collect();
        let artist_views: Vec<&String> = self.artist_views.keys().collect();
        let sleep = json!({
            "secs_left": self.sleep_at.map(|t| t.saturating_duration_since(Instant::now()).as_secs()),
            "end_of_track": self.sleep_end_of_track,
        });
        let phases_n = self.frame_hist.len().max(1) as f32;
        let phases_avg = json!({"ui": self.frame_phases[0] / phases_n, "tessellate": self.frame_phases[1] / phases_n, "raster": self.frame_phases[2] / phases_n, "present": self.frame_phases[3] / phases_n});
        json!({
            "version": crate::update::current_version(),
            "auth": auth,
            "user": self.user.as_ref().map(|u| json!({"id": u.id, "name": u.display_name, "product": u.product})),
            "device_id": self.device_id,
            "page": page_spec(self.page()),
            "active": match self.active { ActiveTab::Home => "home".to_string(), ActiveTab::Search => "search".to_string(), ActiveTab::Tab(i) => i.to_string() },
            "tabs": self.tabs.iter().map(|t| json!({"page": page_spec(t.page()), "history": t.history.len(), "idx": t.idx})).collect::<Vec<_>>(),
            "can_back": self.can_back(),
            "can_forward": self.can_forward(),
            "status": self.status.as_ref().map(|(t, at, e)| json!({"text": t, "error": e, "age_ms": at.elapsed().as_millis() as u64})),
            "player": player,
            "devices": self.devices.iter().map(|d| json!({"name": d.name, "id": d.id, "active": d.is_active, "kind": d.kind, "self": d.id.as_deref() == Some(self.device_id.as_str())})).collect::<Vec<_>>(),
            "queue": queue,
            "queue_source": self.now_context().map(|(name, page)| json!({"name": name, "page": page.as_ref().map(page_spec)})),
            "queued_local": self.queued_local,
            "restoring": self.restore_pending.is_some() || self.restore_wanted || self.restore_awaiting || self.restore_mark || (!self.restore_decided && !self.cluster_restored && self.auth_signed_in_for_control()),
            "lyrics": lyrics,
            "lyrics_for": self.lyrics_for,
            "lyrics_loading": self.lyrics_loading,
            "side": self.side.map(|t| match t { SideTab::Queue => "queue", SideTab::Lyrics => "lyrics" }),
            "playlists": playlists,
            "playlists_loaded": self.playlists_loaded,
            "playlist_meta": self.playlist_meta.iter().map(|(k, p)| (k.clone(), json!({"name": p.name, "public": p.public, "collaborative": p.collaborative, "description": p.description, "tracks_total": p.tracks.as_ref().map(|t| t.total)}))).collect::<serde_json::Map<_, _>>(),
            "lists": lists,
            "liked_count": self.liked_set.len(),
            "saved_albums": self.saved_albums.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            "followed_artists": self.followed_artists.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            "artists_loaded": self.artists_loaded,
            "following": self.following,
            "albums": albums,
            "artists": artists,
            "artist_views": artist_views,
            "users": users,
            "user_playlists": self.user_playlists.iter().map(|(k, v)| (k.clone(), json!(v.len()))).collect::<serde_json::Map<_, _>>(),
            "search": {"query": self.search_query, "loading": self.search_loading, "filter": self.search_filter, "result": search},
            "focus_search": self.focus_search,
            "folders": self.folders.iter().map(|f| json!({"id": f.id, "name": f.name, "playlists": f.playlists})).collect::<Vec<_>>(),
            "settings": serde_json::to_value(&self.settings).unwrap_or(Value::Null),
            "draft_dirty": self.draft != self.settings,
            "editor": self.editor.as_ref().map(|e| json!({"id": e.id, "name": e.name, "public": e.public, "collaborative": e.collaborative, "busy": e.busy})),
            "add_dialog": self.add_dialog.as_ref().map(|d| json!({"uris": d.uris, "selected": d.selected.len(), "pending_new": d.pending_new})),
            "folder_dialog": self.folder_dialog.as_ref().map(|d| json!({"id": d.id, "name": d.name, "busy": d.busy})),
            "jam": {"open": self.jam_open, "busy": self.jam_busy, "error": self.jam_error, "session": self.jam.as_ref().map(|j| json!({"id": j.session_id, "owner": j.is_owner, "members": j.members.len(), "join_url": j.join_url}))},
            "miniplayer": self.miniplayer,
            "fullscreen": self.fullscreen,
            "sidebar_visible": self.settings.sidebar_visible,
            "library_grid": self.library_grid,
            "library_sort_name": self.library_sort_name,
            "library_filter": self.library_filter,
            "home_filter": self.home_filter,
            "artist_tab": self.artist_tab,
            "artist_grid": self.artist_grid,
            "hidden_tracks": self.hidden_tracks,
            "downloaded": self.downloaded,
            "downloading": self.downloading,
            "sleep": sleep,
            "update": {"available": self.update.as_ref().map(|u| u.version.clone()), "banner": self.update_banner, "busy": self.update_busy, "note": self.update_note, "progress": self.update_progress.as_ref().map(|p| p.label()), "failed": matches!(self.update_progress, Some(InstallProgress::Failed(_)))},
            "invite_links": self.invite_links,
            "members": self.members.iter().map(|(k, v)| (k.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "home_feed": self.home_feed.iter().map(|s| json!({"id": s.id, "title": s.title, "items": s.items.len(), "first": s.items.first().map(|i| json!({"uri": i.uri, "title": i.title, "context": i.context}))})).collect::<Vec<_>>(),
            "recent": self.recent.len(),
            "play_log": self.play_log.entries.len(),
            "play_log_last": self.play_log.entries.iter().max_by_key(|e| e.last).map(|e| json!({"uri": e.track.uri, "name": e.track.name, "count": e.count, "last": e.last})),
            "shows": self.shows.iter().map(|(k, (s, eps))| (k.clone(), json!({"name": s.name, "episodes": eps.len()}))).collect::<serde_json::Map<_, _>>(),
            "followed_shows": self.followed_shows,
            "saved_episodes": self.saved_episodes,
            "audiobooks": self.audiobooks.len(),
            "show_shortcuts": self.show_shortcuts,
            "requested": self.requested,
            "web_busy": self.web_busy,
            "ephemeral": self.ephemeral,
            "mem_mb": self.mem_mb,
            "frame_ms": self.frame_ms,
            "frames": {
                "count": self.frame_hist.len(),
                "avg_ms": if self.frame_hist.is_empty() { 0.0 } else { self.frame_hist.iter().sum::<f32>() / self.frame_hist.len() as f32 },
                "max_ms": self.frame_hist.iter().cloned().fold(0.0f32, f32::max),
                "phases_avg_ms": phases_avg,
            },
            "startup": {
                "first_frame_ms": crate::FIRST_FRAME_MS.get().copied(),
                "visible_ms": crate::VISIBLE_MS.get().copied(),
                "since_main_ms": crate::since_start_ms(),
            },
        })
    }
}
