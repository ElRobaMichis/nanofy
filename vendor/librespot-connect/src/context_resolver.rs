use crate::{
    core::{Error, Session, error::ErrorKind},
    protocol::{
        autoplay_context_request::AutoplayContextRequest, context::Context,
        transfer_state::TransferState,
    },
    retry::{self, Retries},
    state::{ConnectState, context::ContextType},
};
use std::{
    cmp::PartialEq,
    collections::{HashMap, VecDeque},
    fmt::{Display, Formatter},
    hash::Hash,
    time::Duration,
};
use thiserror::Error as ThisError;
use tokio::time::Instant;

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
enum Resolve {
    Uri(String),
    Context(Context),
    /// Nanofy: la página siguiente de una sesión del DJ (`next_page_url`), que se añade al final.
    Page(String),
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) enum ContextAction {
    Append,
    Replace,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct ResolveContext {
    resolve: Resolve,
    fallback: Option<String>,
    update: ContextType,
    action: ContextAction,
}

/// Lo que identifica un reintento (Nanofy): todo menos la URI de reserva, que cambia con cada
/// canción (el autoplay de un álbum se pide con la que suena) y haría del mismo autoplay dos
/// reintentos.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct RetryKey {
    resolve: Resolve,
    update: ContextType,
    action: ContextAction,
}

impl ResolveContext {
    fn retry_key(&self) -> RetryKey {
        RetryKey {
            resolve: self.resolve.clone(),
            update: self.update,
            action: self.action.clone(),
        }
    }

    fn append_context(uri: impl Into<String>) -> Self {
        Self {
            resolve: Resolve::Uri(uri.into()),
            fallback: None,
            update: ContextType::Default,
            action: ContextAction::Append,
        }
    }

    /// La página siguiente de la sesión del DJ que suena.
    pub fn next_page(url: impl Into<String>) -> Self {
        Self {
            resolve: Resolve::Page(url.into()),
            fallback: None,
            update: ContextType::Default,
            action: ContextAction::Append,
        }
    }

    pub fn from_uri(
        uri: impl Into<String>,
        fallback: impl Into<String>,
        update: ContextType,
        action: ContextAction,
    ) -> Self {
        let fallback_uri = fallback.into();
        Self {
            resolve: Resolve::Uri(uri.into()),
            fallback: (!fallback_uri.is_empty()).then_some(fallback_uri),
            update,
            action,
        }
    }

    pub fn from_context(context: Context, update: ContextType, action: ContextAction) -> Self {
        Self {
            resolve: Resolve::Context(context),
            fallback: None,
            update,
            action,
        }
    }

    /// the uri which should be used to resolve the context, might not be the context uri
    fn resolve_uri(&self) -> Option<&str> {
        // it's important to call this always, or at least for every ResolveContext
        // otherwise we might not even check if we need to fallback and just use the fallback uri
        match self.resolve {
            Resolve::Uri(ref uri) => ConnectState::valid_resolve_uri(uri),
            Resolve::Context(ref ctx) => {
                ConnectState::find_valid_uri(ctx.uri.as_deref(), ctx.pages.first())
            }
            Resolve::Page(ref url) => Some(url.as_str()),
        }
        .or(self.fallback.as_deref())
    }

    /// the actual context uri
    fn context_uri(&self) -> &str {
        match self.resolve {
            Resolve::Uri(ref uri) | Resolve::Page(ref uri) => uri,
            Resolve::Context(ref ctx) => ctx.uri.as_deref().unwrap_or_default(),
        }
    }

    /// Qué contexto actualiza al resolverse: el principal o el del autoplay.
    pub fn update(&self) -> ContextType {
        self.update
    }
}

impl Display for ResolveContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "resolve_uri: <{:?}>, context_uri: <{}>, update: <{:?}>",
            self.resolve_uri(),
            self.context_uri(),
            self.update,
        )
    }
}

#[derive(Debug, ThisError)]
enum ContextResolverError {
    #[error("no next context to resolve")]
    NoNext,
    #[error("tried appending context with {0} pages")]
    UnexpectedPagesSize(usize),
    #[error("tried resolving not allowed context: {0:?}")]
    NotAllowedContext(String),
}

impl From<ContextResolverError> for Error {
    fn from(value: ContextResolverError) -> Self {
        Error::failed_precondition(value)
    }
}

/// ¿Puede salir bien si se pide otra vez? Casi todo lo que falla al resolver un contexto es la red
/// o Spotify un momento (sin respuesta, 5xx, límite de peticiones, token caducado). No: lo que
/// Spotify dice que no existe o no está permitido, y lo que se descarta aquí mismo (el autoplay de
/// un podcast).
fn is_transient(err: &Error) -> bool {
    if err.error.downcast_ref::<ContextResolverError>().is_some() {
        return false;
    }
    !matches!(
        err.kind,
        ErrorKind::NotFound
            | ErrorKind::PermissionDenied
            | ErrorKind::InvalidArgument
            | ErrorKind::OutOfRange
            | ErrorKind::Unimplemented
    )
}

pub struct ContextResolver {
    session: Session,
    queue: VecDeque<ResolveContext>,
    unavailable_contexts: HashMap<ResolveContext, Instant>,
    /// Nanofy: lo que falló por algo pasajero, a la espera de volver a pedirse (ver `retry`).
    /// Antes iba a `unavailable_contexts` y, al acabarse la cola, la música se paraba.
    retries: Retries<RetryKey, ResolveContext>,
}

// time after which an unavailable context is retried
const RETRY_UNAVAILABLE: Duration = Duration::from_secs(3600);

/// Metadato de un contexto que no se resuelve por context-resolve (el DJ).
const LEXICON_CONTEXT_URL: &str = "lexicon_context_url";

impl ContextResolver {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            queue: VecDeque::new(),
            unavailable_contexts: HashMap::new(),
            retries: Retries::new(&retry::CONTEXT_STEPS),
        }
    }

    pub fn add(&mut self, resolve: ResolveContext) {
        // Nanofy: antes `marca.duration_since(ahora)`, que con la marca en el pasado siempre da
        // cero: un contexto no disponible no se volvía a pedir nunca, en vez de a la hora.
        if let Some(marked) = self.unavailable_contexts.get(&resolve) {
            let ago = marked.elapsed();
            if ago < RETRY_UNAVAILABLE {
                debug!("tried loading unavailable context: {resolve}");
                return;
            }
            let _ = self.unavailable_contexts.remove(&resolve);
            debug!(
                "context was requested {}s ago, trying again to resolve the requested context",
                ago.as_secs()
            );
        }

        // Ya tiene un reintento pendiente (programado o de camino): pedirlo también ahora sería
        // otra petición a la vez (esto se vuelve a pedir con cada canción que empieza).
        if self.retries.pending(&resolve.retry_key()) {
            debug!("{resolve} ya tiene un reintento pendiente");
            return;
        } else if self.queue.contains(&resolve) {
            debug!("update for {resolve} is already added");
            return;
        } else {
            trace!(
                "added {} to resolver queue",
                resolve.resolve_uri().unwrap_or(resolve.context_uri())
            )
        }

        self.queue.push_back(resolve)
    }

    /// Como `add`, para lo que pide el usuario (cargar un contexto): se intenta ya, aunque haya
    /// fallado antes o espere su reintento.
    pub fn add_now(&mut self, resolve: ResolveContext) {
        let _ = self.unavailable_contexts.remove(&resolve);
        self.retries.forget(&resolve.retry_key());
        self.add(resolve)
    }

    pub fn add_list(&mut self, resolve: Vec<ResolveContext>) {
        for resolve in resolve {
            self.add(resolve)
        }
    }

    pub fn remove_used_and_invalid(&mut self) {
        if let Some((_, _, remove)) = self.find_next() {
            let _ = self.queue.drain(0..remove); // remove invalid
        }
        self.queue.pop_front(); // remove used
    }

    /// Vacía la cola y olvida los reintentos pendientes: eran de lo que sonaba hasta ahora.
    pub fn clear(&mut self) {
        self.queue = VecDeque::new();
        self.retries.clear();
    }

    fn find_next(&self) -> Option<(&ResolveContext, &str, usize)> {
        for idx in 0..self.queue.len() {
            let next = self.queue.get(idx)?;
            match next.resolve_uri() {
                None => {
                    warn!("skipped {idx} because of invalid resolve_uri: {next}");
                    continue;
                }
                Some(uri) => return Some((next, uri, idx)),
            }
        }
        None
    }

    pub fn has_next(&self) -> bool {
        self.find_next().is_some()
    }

    /// ¿Lo siguiente por resolver es otra página del contexto principal? (Nanofy: Spirc las trae
    /// enseguida cuando la canción pedida aún no ha aparecido, ver `start_index`).
    pub fn next_is_default_page(&self) -> bool {
        self.find_next().is_some_and(|(next, _, _)| {
            next.update == ContextType::Default && next.action == ContextAction::Append
        })
    }

    pub async fn get_next_context(
        &self,
        recent_track_uri: impl Fn() -> Vec<String>,
    ) -> Result<Context, Error> {
        let (next, resolve_uri, _) = self.find_next().ok_or(ContextResolverError::NoNext)?;

        if let Resolve::Page(ref url) = next.resolve {
            let page = self.session.spclient().get_context_page(url).await?;
            debug!(
                "página siguiente del DJ: {} canciones",
                page.tracks.len()
            );
            return Ok(Context {
                pages: vec![page],
                ..Default::default()
            });
        }

        match next.update {
            ContextType::Default => {
                let spclient = self.session.spclient();
                let mut ctx = spclient.get_context(resolve_uri).await;
                // El DJ: context-resolve da una página sin canciones ni URL y nombra en los
                // metadatos la sesión de la que salen de verdad (con los textos del locutor).
                let lexicon = ctx.as_ref().ok().and_then(|c| {
                    let empty = c.pages.iter().all(|p| {
                        p.tracks.is_empty() && p.page_url.as_deref().unwrap_or_default().is_empty()
                    });
                    c.metadata
                        .get(LEXICON_CONTEXT_URL)
                        .filter(|url| empty && url.starts_with("hm://"))
                        .cloned()
                });
                if let Some(url) = lexicon {
                    debug!("<{resolve_uri}> llega vacío; sus canciones se piden a {url}");
                    let first = ctx.expect("comprobado arriba");
                    ctx = spclient.get_context_from_url(&url).await.map(|mut real| {
                        for (key, value) in first.metadata {
                            real.metadata.entry(key).or_insert(value);
                        }
                        real
                    });
                }
                if let Ok(ctx) = ctx.as_mut() {
                    ctx.uri = Some(next.context_uri().to_string());
                    // La sesión del DJ conserva su URL hm://: así la reconocen los demás
                    // dispositivos de Connect.
                    if !ctx.url.as_deref().is_some_and(|u| u.starts_with("hm://")) {
                        ctx.url = ctx.uri.as_ref().map(|s| format!("context://{s}"));
                    }
                }

                ctx
            }
            ContextType::Autoplay => {
                if resolve_uri.contains("spotify:show:") || resolve_uri.contains("spotify:episode:")
                {
                    // autoplay is not supported for podcasts
                    Err(ContextResolverError::NotAllowedContext(
                        resolve_uri.to_string(),
                    ))?
                }

                let request = AutoplayContextRequest {
                    context_uri: Some(resolve_uri.to_string()),
                    recent_track_uri: recent_track_uri(),
                    ..Default::default()
                };
                self.session.spclient().get_autoplay_context(&request).await
            }
        }
    }

    /// Lo siguiente de la cola no se pudo resolver (`why`) mientras sonaba `owner`. Un fallo
    /// pasajero se reintenta más tarde y devuelve la espera; uno definitivo (no existe, no está
    /// permitido) lo deja no disponible como antes (`None`). Sale de la cola igualmente, con
    /// `remove_used_and_invalid`.
    pub fn next_failed(&mut self, why: &Error, owner: &str) -> Option<Duration> {
        let next = self.find_next()?.0.clone();
        let key = next.retry_key();
        if is_transient(why) {
            let at_least = retry::rate_limit_wait(&why.to_string());
            let now = std::time::Instant::now();
            Some(self.retries.failed(key, next, owner, now, at_least))
        } else {
            self.retries.forget(&key);
            self.unavailable_contexts.insert(next, Instant::now());
            None
        }
    }

    /// Lo siguiente de la cola se resolvió: si tenía un reintento pendiente (este u otro igual con
    /// otra reserva), se olvida, para no pedirlo otra vez.
    pub fn next_succeeded(&mut self) {
        if let Some(next) = self.find_next().map(|(next, _, _)| next.clone()) {
            if self.retries.succeeded(&next.retry_key()) {
                debug!("{next} se resolvió al reintentarlo");
            }
        }
    }

    /// El próximo reintento programado (ver `requeue_due`).
    pub fn next_retry_at(&self) -> Option<std::time::Instant> {
        self.retries.next_at()
    }

    /// Vuelve a poner en la cola los reintentos a los que ya les toca, si aún hacen falta según
    /// `needed(contexto, lo que sonaba al fallar)`; los que no, se olvidan.
    pub fn requeue_due(&mut self, needed: impl Fn(&ResolveContext, &str) -> bool) {
        for (key, resolve, owner) in self.retries.take_due(std::time::Instant::now()) {
            if !needed(&resolve, &owner) {
                debug!("ya no hace falta reintentar {resolve}");
                self.retries.forget(&key);
            } else if !self.queue.contains(&resolve) {
                debug!(
                    "se reintenta {resolve} ({} fallos)",
                    self.retries.failures(&key)
                );
                self.queue.push_back(resolve);
            }
        }
    }

    /// Se acabaron las canciones esperando un reintento: se adelanta (ver `retry::HURRY`).
    pub fn hurry_retries(&mut self) {
        self.retries.hurry(std::time::Instant::now(), retry::HURRY);
    }

    /// ¿Pueden venir más canciones para `owner` (lo que suena)? Lo que haya en la cola (el
    /// autoplay, la página siguiente del DJ, otra página o el contexto entero de nuevo), o lo que
    /// falló mientras sonaba y espera su reintento.
    pub fn more_tracks_pending(&self, owner: &str) -> bool {
        self.has_next() || self.retries.items_for(owner).next().is_some()
    }

    pub fn apply_next_context(
        &self,
        state: &mut ConnectState,
        mut context: Context,
    ) -> Result<Option<Vec<ResolveContext>>, Error> {
        let (next, _, _) = self.find_next().ok_or(ContextResolverError::NoNext)?;

        let remaining = match next.action {
            // Nanofy: más autoplay va al contexto de autoplay, no al principal (ver
            // `fill_autoplay_from_page`).
            ContextAction::Append if context.pages.len() == 1 => match next.update {
                // Pedido para lo que sonaba antes (una transferencia a otro contexto mientras
                // llegaba): ya no sirve.
                ContextType::Autoplay if next.context_uri() != state.context_uri().as_str() => {
                    debug!("autoplay de otro contexto ({}), se descarta", next.context_uri());
                    Ok(None)
                }
                ContextType::Autoplay if state.get_context(ContextType::Autoplay).is_ok() => state
                    .fill_autoplay_from_page(context.pages.remove(0))
                    .map(|_| None),
                // El autoplay se descartó mientras se pedía (se volvió a cargar el contexto con
                // el autoplay ya sonando, ver `reset_context`): llega como uno nuevo.
                ContextType::Autoplay => state.update_context(context, ContextType::Autoplay),
                ContextType::Default => state
                    .fill_context_from_page(context.pages.remove(0))
                    .map(|_| None),
            },
            ContextAction::Replace => {
                let remaining = state.update_context(context, next.update);
                if let Resolve::Context(ref ctx) = next.resolve {
                    state.merge_context(ctx.pages.clone().pop());
                }

                remaining
            }
            ContextAction::Append => {
                warn!("unexpected page size: {context:#?}");
                Err(ContextResolverError::UnexpectedPagesSize(context.pages.len()).into())
            }
        }?;

        Ok(remaining.map(|remaining| {
            remaining
                .into_iter()
                .map(ResolveContext::append_context)
                .collect::<Vec<_>>()
        }))
    }

    pub fn try_finish(
        &self,
        state: &mut ConnectState,
        transfer_state: &mut Option<TransferState>,
    ) -> bool {
        let (next, _, _) = match self.find_next() {
            None => return false,
            Some(next) => next,
        };

        // when there is only one update type, we are the last of our kind, so we should update the state
        if self
            .queue
            .iter()
            .filter(|resolve| resolve.update == next.update)
            .count()
            != 1
        {
            return false;
        }

        match (next.update, state.active_context) {
            (ContextType::Default, ContextType::Default) | (ContextType::Autoplay, _) => {
                debug!(
                    "last item of type <{:?}>, finishing state setup",
                    next.update
                );
            }
            (ContextType::Default, _) => {
                debug!("skipped finishing default, because it isn't the active context");
                return false;
            }
        }

        let active_ctx = state.get_context(state.active_context);
        let res = if let Some(transfer_state) = transfer_state.take() {
            state.finish_transfer(transfer_state)
        } else if state.shuffling_context() && next.update == ContextType::Default {
            state.shuffle_new()
        } else if matches!(active_ctx, Ok(ctx) if ctx.index.track == 0) {
            // has context, and context is not touched
            // when the index is not zero, the next index was already evaluated elsewhere
            let ctx = active_ctx.expect("checked by precondition");
            let idx = ConnectState::find_index_in_context(ctx, |t| {
                state.current_track(|c| t.uri == c.uri)
            })
            .ok();

            state.reset_playback_to_position(idx)
        } else {
            state.fill_up_next_tracks()
        };

        if let Err(why) = res {
            error!("setup of state failed: {why}, last used resolve {next:#?}")
        }

        state.update_restrictions();
        state.update_queue_revision();

        true
    }
}
