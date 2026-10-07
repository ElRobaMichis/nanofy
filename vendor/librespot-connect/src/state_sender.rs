//! Envío en segundo plano del estado de Spotify Connect (Nanofy; ver `state_queue`).
//!
//! Spirc copia el estado en el momento y sigue con lo suyo; una tarea aparte hace los PUT de uno
//! en uno, juntando los que se acumulan mientras uno va de camino. Así un PUT colgado (hasta que
//! lo corta su plazo, ver `request_policy` en librespot-core) ya no frena Pausa ni Siguiente.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::{Notify, oneshot, watch};

use crate::{
    core::{Error, Session, spclient::SpClientResult},
    protocol::connect::{PutStateReason, PutStateRequest},
    state_queue::StateQueue,
};

struct Job {
    request: PutStateRequest,
    /// Quien espera la respuesta: el registro del dispositivo, que necesita el clúster que
    /// devuelve Spotify.
    reply: Option<oneshot::Sender<SpClientResult>>,
}

struct Shared {
    queue: Mutex<StateQueue<Job>>,
    wake: Notify,
}

impl Shared {
    fn queue(&self) -> std::sync::MutexGuard<'_, StateQueue<Job>> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Lo que Spirc usa para avisar a Spotify de su estado sin esperar a la red.
pub(crate) struct StateSender {
    shared: Arc<Shared>,
    /// Número del último aviso que ya salió (o que falló): `flush` espera a que llegue al último
    /// encolado. Si la tarea muriera, el canal se cierra y nadie se queda esperando.
    sent: watch::Receiver<u64>,
    worker: tokio::task::JoinHandle<()>,
}

impl StateSender {
    /// Lanza la tarea que envía, en el runtime de quien llama (el de Spirc).
    pub fn new(session: Session) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(StateQueue::new()),
            wake: Notify::new(),
        });
        let (sent_tx, sent) = watch::channel(0);
        let worker = tokio::spawn(run(shared.clone(), session, sent_tx));
        Self {
            shared,
            sent,
            worker,
        }
    }

    fn push(&self, job: Job, replaceable: bool) {
        self.shared.queue().push(job, replaceable);
        // Si la tarea no está esperando, queda un aviso guardado: no se pierde nunca uno.
        self.shared.wake.notify_one();
    }

    /// Envía `request` en cuanto se pueda, sin esperar. Un estado normal que aún no ha salido
    /// cuando llega otro se sustituye por el nuevo; uno con motivo propio (volumen) sale tal cual.
    pub fn send(&self, request: PutStateRequest) {
        let replaceable = is_plain(&request);
        self.push(
            Job {
                request,
                reply: None,
            },
            replaceable,
        );
    }

    /// Envía `request` detrás de lo que ya esté en cola y espera la respuesta de Spotify. Nunca se
    /// sustituye: quien llama necesita su respuesta.
    pub async fn send_and_wait(&self, request: PutStateRequest) -> SpClientResult {
        let (tx, rx) = oneshot::channel();
        self.push(
            Job {
                request,
                reply: Some(tx),
            },
            false,
        );
        rx.await
            .unwrap_or_else(|_| Err(Error::aborted("connect state: el envío se interrumpió")))
    }

    /// Espera a que salga todo lo encolado hasta ahora. Antes de pasar a inactivo o de borrar el
    /// dispositivo: un estado que aún esperara llegaría después y lo desharía.
    pub async fn flush(&self) {
        let target = self.shared.queue().last_seq();
        let mut sent = self.sent.clone();
        let _ = sent.wait_for(|s| *s >= target).await;
    }
}

impl Drop for StateSender {
    /// Spirc terminó: lo que quede ya no le importa a nadie (antes de terminar a propósito, Spirc
    /// espera con `flush`).
    fn drop(&mut self) {
        self.worker.abort();
    }
}

/// Un estado sin motivo propio (el de cada cambio), que el siguiente puede sustituir.
fn is_plain(request: &PutStateRequest) -> bool {
    request.put_state_reason.enum_value() == Ok(PutStateReason::PLAYER_STATE_CHANGED)
}

async fn run(shared: Arc<Shared>, session: Session, sent: watch::Sender<u64>) {
    loop {
        let next = shared.queue().pop();
        let Some(job) = next else {
            shared.wake.notified().await;
            continue;
        };
        let started = std::time::Instant::now();
        let result = session
            .spclient()
            .put_connect_state_request(&job.item.request)
            .await;
        match &result {
            Ok(_) => trace!(
                "connect state {} enviado en {} ms",
                job.seq,
                started.elapsed().as_millis()
            ),
            Err(e) => error!("could not send connect state: {e}"),
        }
        if let Some(reply) = job.item.reply {
            let _ = reply.send(result);
        }
        sent.send_replace(job.seq);
    }
}
