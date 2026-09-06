//! Response-scoped cancellation. Registration precedes the started event so even
//! an immediate Stop has a live target; the guard removes completed requests.
use std::{collections::HashMap, sync::Mutex};
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Default)]
pub struct Responses(Mutex<HashMap<Uuid, Option<oneshot::Sender<()>>>>);

pub struct ResponseGuard<'a> {
    responses: &'a Responses,
    id: Uuid,
}
impl Responses {
    pub fn register(&self, id: Uuid) -> Result<(ResponseGuard<'_>, oneshot::Receiver<()>), String> {
        let mut runs = self.0.lock().expect("response registry");
        if runs.contains_key(&id) {
            return Err("response already running".into());
        }
        let (sender, receiver) = oneshot::channel();
        runs.insert(id, Some(sender));
        Ok((
            ResponseGuard {
                responses: self,
                id,
            },
            receiver,
        ))
    }
    pub fn cancel(&self, id: Uuid) {
        if let Some(sender) = self
            .0
            .lock()
            .expect("response registry")
            .get_mut(&id)
            .and_then(Option::take)
        {
            let _ = sender.send(());
        }
    }
}
impl Drop for ResponseGuard<'_> {
    fn drop(&mut self) {
        self.responses
            .0
            .lock()
            .expect("response registry")
            .remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_is_scoped_and_registration_is_cleaned_up() {
        let runs = Responses::default();
        let id = Uuid::new_v4();
        let (guard, cancelled) = runs.register(id).unwrap();
        let (_other, mut other_cancelled) = runs.register(Uuid::new_v4()).unwrap();
        assert!(runs.register(id).is_err());
        runs.cancel(id);
        cancelled.await.unwrap();
        assert!(matches!(
            other_cancelled.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        drop(guard);
        assert!(runs.register(id).is_ok());
    }
}
