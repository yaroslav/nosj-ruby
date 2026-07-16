//! Pre-escaped object-key cache for generation.

use rb_sys::VALUE;

use crate::state::{ensure_marked_shadow, VStackShadow};

/// Epoch eviction, like the parse-side key cache: at capacity the whole map
/// is cleared and hot keys repopulate; one unique-key-heavy document must
/// not degrade every later document.
const GEN_KEY_CACHE_CAP: usize = 2048;

/// Cache of quoted-and-escaped key bytes, keyed on the key string's VALUE
/// identity. Sound because Ruby freezes all string hash keys (immutable
/// content) and the GC-marked shadow keeps every cached key alive (an
/// address can never be reused for a different string while cached).
/// Documents emit few unique keys many times over (twitter: 94 unique keys,
/// 13,345 emissions), so key emission collapses to one short copy.
///
/// Taken out of the thread-local for the duration of a generate call (like
/// the output buffer): recursive generation via a user `to_json` sees a
/// fresh default and cannot double-borrow.
#[derive(Default)]
pub(super) struct GenKeyCache {
    map: ahash::AHashMap<VALUE, Box<[u8]>>,
    shadow: Option<&'static mut VStackShadow>,
}

impl GenKeyCache {
    pub(super) fn with_capacity(n: usize) -> Self {
        GenKeyCache {
            map: ahash::AHashMap::with_capacity(n),
            shadow: None,
        }
    }

    #[inline]
    pub(super) fn get(&self, k: VALUE) -> Option<&[u8]> {
        self.map.get(&k).map(Box::as_ref)
    }

    /// Cache `bytes` for `k`, evicting the whole epoch at capacity and
    /// registering `k` with the GC-marked shadow that keeps it alive.
    pub(super) fn store(&mut self, k: VALUE, bytes: Box<[u8]>) {
        if self.map.len() >= GEN_KEY_CACHE_CAP {
            self.map.clear();
            if let Some(shadow) = self.shadow.as_mut() {
                shadow.values.clear();
            }
        }
        ensure_marked_shadow(&mut self.shadow);
        self.shadow
            .as_mut()
            .expect("shadow just ensured")
            .values
            .push(k);
        self.map.insert(k, bytes);
    }
}
