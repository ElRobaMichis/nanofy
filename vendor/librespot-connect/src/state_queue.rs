//! Cola de los avisos de estado de Spotify Connect (Nanofy).
//!
//! Spirc cuenta cada cambio a Spotify con un PUT a connect-state (~120 ms, o mucho más con la red
//! mal). Antes cada orden esperaba al suyo antes de atender la siguiente: con un PUT lento o
//! colgado, Pausa y Siguiente se quedaban en cola detrás. Ahora el estado se copia en el momento
//! y lo envía una tarea aparte, de uno en uno (`state_sender`). Mientras uno va de camino, los
//! siguientes se juntan: cada estado sustituye entero al anterior, así que de los que esperan solo
//! sale el último. Los que llevan un motivo propio (dispositivo nuevo, cambio de volumen) no se
//! sustituyen nunca: Spotify los necesita tal cual y en orden.
//!
//! No usa nada del resto del crate, para que sus pruebas corran en el binario de Nanofy.

use std::collections::VecDeque;

/// Un aviso en cola con su número de envío. Los números crecen hacia el final de la cola: cuando
/// sale (o se da por perdido) el de número `n`, todo lo encolado hasta `n` ya está cubierto.
#[derive(Debug, PartialEq, Eq)]
pub struct Queued<T> {
    pub seq: u64,
    pub item: T,
    /// Un estado normal, que el siguiente puede sustituir mientras espera.
    pub replaceable: bool,
}

/// Avisos pendientes de enviar, del más antiguo al más nuevo.
#[derive(Debug)]
pub struct StateQueue<T> {
    pending: VecDeque<Queued<T>>,
    last_seq: u64,
}

impl<T> Default for StateQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> StateQueue<T> {
    pub const fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            last_seq: 0,
        }
    }

    /// Encola un aviso y devuelve su número. Uno `replaceable` sustituye al último pendiente si
    /// también lo es (y se queda con el número nuevo): de dos estados seguidos solo cuenta el
    /// último. El que va de camino ya salió de la cola y no se toca.
    pub fn push(&mut self, item: T, replaceable: bool) -> u64 {
        self.last_seq += 1;
        let seq = self.last_seq;
        if replaceable {
            if let Some(last) = self.pending.back_mut().filter(|q| q.replaceable) {
                last.item = item;
                last.seq = seq;
                return seq;
            }
        }
        self.pending.push_back(Queued {
            seq,
            item,
            replaceable,
        });
        seq
    }

    /// El siguiente que hay que enviar.
    pub fn pop(&mut self) -> Option<Queued<T>> {
        self.pending.pop_front()
    }

    /// Número del último encolado (0 si nunca se encoló nada): esperar a que salga ese es esperar
    /// a todo lo anterior.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(q: &mut StateQueue<&'static str>) -> Vec<(u64, &'static str)> {
        std::iter::from_fn(|| q.pop().map(|j| (j.seq, j.item))).collect()
    }

    #[test]
    fn de_varios_estados_seguidos_solo_sale_el_ultimo() {
        let mut q = StateQueue::new();
        // Siguiente, siguiente, pausa mientras el primero va de camino: un solo PUT más.
        assert_eq!(q.push("siguiente 1", true), 1);
        let en_vuelo = q.pop().unwrap();
        assert_eq!((en_vuelo.seq, en_vuelo.item), (1, "siguiente 1"));
        q.push("siguiente 2", true);
        q.push("siguiente 3", true);
        assert_eq!(q.push("pausa", true), 4);
        assert_eq!(q.len(), 1);
        assert_eq!(drain(&mut q), vec![(4, "pausa")]);
        assert!(q.is_empty());
    }

    #[test]
    fn los_avisos_con_motivo_no_se_sustituyen_y_van_en_orden() {
        let mut q = StateQueue::new();
        q.push("estado A", true);
        q.push("volumen", false);
        // Lo que llega detrás del volumen no puede sustituirlo ni adelantarlo…
        q.push("estado B", true);
        // …pero sí sustituye al estado que espera detrás de él.
        q.push("estado C", true);
        q.push("dispositivo nuevo", false);
        q.push("dispositivo nuevo", false);
        assert_eq!(
            drain(&mut q),
            vec![
                (1, "estado A"),
                (2, "volumen"),
                (4, "estado C"),
                (5, "dispositivo nuevo"),
                (6, "dispositivo nuevo"),
            ]
        );
    }

    #[test]
    fn los_numeros_crecen_hacia_el_final_aunque_se_sustituya() {
        // Esperar a `last_seq` cubre todo: cuando sale el de ese número no queda nada anterior.
        let mut q = StateQueue::new();
        assert_eq!(q.last_seq(), 0);
        for i in 0..20 {
            q.push(if i % 3 == 0 { "motivo" } else { "estado" }, i % 3 != 0);
        }
        let last = q.last_seq();
        assert_eq!(last, 20);
        let seqs: Vec<u64> = drain(&mut q).into_iter().map(|(s, _)| s).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
        assert_eq!(seqs.last(), Some(&last));
    }
}
