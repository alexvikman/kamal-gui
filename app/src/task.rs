use std::sync::mpsc;

/// A blocking job on a background thread. Poll it each frame; the thread
/// wakes the UI when it finishes.
pub struct Task<T> {
    rx: mpsc::Receiver<Result<T, String>>,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f().map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        Self { rx }
    }

    pub fn poll(&mut self) -> Option<Result<T, String>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("background task crashed".into())),
        }
    }
}

/// Take the result of an optional task once it's done.
pub fn poll<T: Send + 'static>(slot: &mut Option<Task<T>>) -> Option<Result<T, String>> {
    let r = slot.as_mut()?.poll()?;
    *slot = None;
    Some(r)
}
