//! egui bytes loader for `https://` thumbnails, fetched on the backend runtime.
//!
//! Reusing the backend's reqwest client avoids a second HTTP/TLS stack in the binary.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::load::{Bytes, BytesLoadResult, BytesLoader, BytesPoll, LoadError};

use crate::backend::Backend;

enum Entry {
    Pending,
    Ready(Arc<[u8]>, Option<String>),
    Failed(String),
}

pub struct HttpImages {
    backend: Backend,
    cache: Arc<Mutex<HashMap<String, Entry>>>,
}

impl HttpImages {
    pub fn new(backend: Backend) -> Self {
        Self { backend, cache: Default::default() }
    }
}

impl BytesLoader for HttpImages {
    fn id(&self) -> &str {
        egui::generate_loader_id!(HttpImages)
    }

    fn load(&self, _ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        if !uri.starts_with("https://") && !uri.starts_with("http://") {
            return Err(LoadError::NotSupported);
        }
        let mut cache = self.cache.lock().unwrap();
        match cache.get(uri) {
            Some(Entry::Ready(bytes, mime)) => {
                return Ok(BytesPoll::Ready {
                    size: None,
                    bytes: Bytes::Shared(bytes.clone()),
                    mime: mime.clone(),
                });
            }
            Some(Entry::Pending) => return Ok(BytesPoll::Pending { size: None }),
            Some(Entry::Failed(e)) => return Err(LoadError::Loading(e.clone())),
            None => {}
        }
        cache.insert(uri.to_owned(), Entry::Pending);
        drop(cache);

        let (http, cache, uri, backend) =
            (self.backend.http().clone(), self.cache.clone(), uri.to_owned(), self.backend.clone());
        self.backend.spawn(async move {
            let res = async {
                let resp = http.get(&uri).send().await?.error_for_status()?;
                let mime = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_owned);
                Ok::<_, reqwest::Error>((resp.bytes().await?, mime))
            }
            .await;
            let entry = match res {
                Ok((bytes, mime)) => Entry::Ready(Arc::from(&bytes[..]), mime),
                Err(e) => Entry::Failed(e.to_string()),
            };
            // Only store if not forgotten while in flight.
            if let Some(slot) = cache.lock().unwrap().get_mut(&uri) {
                *slot = entry;
            }
            backend.wake();
        });
        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.cache.lock().unwrap().remove(uri);
    }

    fn forget_all(&self) {
        self.cache.lock().unwrap().clear();
    }

    fn byte_size(&self) -> usize {
        self.cache
            .lock()
            .unwrap()
            .values()
            .map(|e| match e {
                Entry::Ready(b, _) => b.len(),
                _ => 0,
            })
            .sum()
    }

    fn has_pending(&self) -> bool {
        self.cache.lock().unwrap().values().any(|e| matches!(e, Entry::Pending))
    }
}
