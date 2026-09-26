//! The live connections of one published port.
//!
//! A published port is served by one thread per connection, and every one of
//! those threads blocks in a read. That leaves the socket as the only way to end
//! a connection, so this module owns the duplicate sockets a withdrawal and an
//! idle reap use to wake a blocked thread.

use super::*;

/// When a proxied connection last moved bytes.
///
/// Both directions share one tracker, so a connection stays alive while either
/// direction is busy. A connection that has been quiet for the idle timeout is
/// closed instead of holding a permit and a worker thread until the port is
/// unpublished, which is what stops abandoned sockets from starving the
/// connection budgets.
pub(crate) struct ConnectionActivity {
    start: Instant,
    last_millis: AtomicU64,
}

impl ConnectionActivity {
    pub(crate) fn new() -> Self {
        Self {
            start: Instant::now(),
            last_millis: AtomicU64::new(0),
        }
    }

    pub(crate) fn touch(&self) {
        self.last_millis
            .store(self.start.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    pub(crate) fn idle_for(&self) -> Duration {
        let elapsed = self.start.elapsed().as_millis() as u64;
        Duration::from_millis(elapsed.saturating_sub(self.last_millis.load(Ordering::Relaxed)))
    }
}

/// The live connections of one published port.
///
/// A connection thread blocks in `read` so that an idle connection costs no CPU,
/// which leaves the socket as the only way to end one: a port withdrawal and an
/// idle reap both work by shutting the connection down, and the blocked read
/// returns as soon as they do. The set holds a duplicate of both sockets for
/// that purpose rather than the raw descriptors, because the kernel reuses a
/// descriptor number as soon as it is closed and shutting down a reused number
/// would end an unrelated connection.
pub(crate) struct ConnectionSet {
    live: Mutex<Vec<LiveConnection>>,
    next_id: AtomicU64,
}

struct LiveConnection {
    id: u64,
    activity: Arc<ConnectionActivity>,
    client: TcpStream,
    workspace: TcpStream,
}

impl ConnectionSet {
    pub(crate) fn new() -> Self {
        Self {
            live: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a connection so it can be woken later.
    ///
    /// Failing to duplicate a socket is not fatal. The connection still carries
    /// data; it simply ends when its peer does instead of being woken early.
    pub(crate) fn register(
        &self,
        activity: Arc<ConnectionActivity>,
        client: &TcpStream,
        workspace: &TcpStream,
    ) -> Option<u64> {
        let client = client.try_clone().ok()?;
        let workspace = workspace.try_clone().ok()?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.live().push(LiveConnection {
            id,
            activity,
            client,
            workspace,
        });
        Some(id)
    }

    pub(crate) fn release(&self, id: u64) {
        self.live().retain(|connection| connection.id != id);
    }

    /// End every connection that has moved no bytes for `idle_timeout`.
    pub(crate) fn shutdown_idle(&self, idle_timeout: Duration) {
        for connection in self.live().iter() {
            if connection.activity.idle_for() >= idle_timeout {
                connection.shutdown();
            }
        }
    }

    /// End every connection, which wakes any thread blocked on its socket.
    pub(crate) fn shutdown_all(&self) {
        for connection in self.live().iter() {
            connection.shutdown();
        }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Vec<LiveConnection>> {
        // The set is a list of sockets, and the worst a stale entry can do is
        // shut down a socket that is already gone, so a panic while the lock is
        // held must not cost the daemon its port handling.
        self.live.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl LiveConnection {
    fn shutdown(&self) {
        let _ = self.client.shutdown(Shutdown::Both);
        let _ = self.workspace.shutdown(Shutdown::Both);
    }
}
