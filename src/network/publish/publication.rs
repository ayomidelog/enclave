use super::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct WorkspacePublishKey {
    pub(super) sandbox_id: String,
    pub(super) workspace_id: String,
}

pub(super) struct ActivePublication {
    spec: PublishedPortSpec,
    workspace_ip: String,
    shutdown: Arc<AtomicBool>,
    connections: Arc<ConnectionSet>,
    accept_thread: Option<JoinHandle<()>>,
}

impl WorkspacePublishKey {
    pub(super) fn new(sandbox_id: &str, workspace_id: &str) -> Self {
        Self {
            sandbox_id: sandbox_id.to_string(),
            workspace_id: workspace_id.to_string(),
        }
    }
}

impl ActivePublication {
    pub(super) fn bind(
        spec: PublishedPortSpec,
        runtime_pid: u32,
        workspace_ip: &str,
        global_connections: Arc<ConnectionLimiter>,
    ) -> Result<Self> {
        let bind_addr = format!("{}:{}", spec.host_ip, spec.host_port);
        let listener =
            TcpListener::bind(&bind_addr).map_err(|err| publish_bind_error(&spec, err))?;
        listener
            .set_nonblocking(true)
            .with_context(|| format!("failed to configure nonblocking listener at {bind_addr}"))?;

        // One budget per published port, plus a share of the daemon-wide budget
        // passed in, so a single busy port cannot consume every slot.
        let connections = ConnectionBudget::new(
            Arc::new(ConnectionLimiter::new(MAX_CONNECTIONS_PER_PUBLISHER)),
            global_connections,
        );

        let shutdown = Arc::new(AtomicBool::new(false));
        let live = Arc::new(ConnectionSet::new());
        let accept_shutdown = shutdown.clone();
        let accept_live = Arc::clone(&live);
        let thread_name = format!("enclave-port-{}-{}", spec.host_port, spec.workspace_port);
        let workspace_port = spec.workspace_port;
        let accept_thread = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run_accept_loop(
                    listener,
                    accept_shutdown,
                    runtime_pid,
                    workspace_port,
                    connections,
                    accept_live,
                )
            })
            .context("failed to spawn published-port accept thread")?;

        Ok(Self {
            spec,
            workspace_ip: workspace_ip.to_string(),
            shutdown,
            connections: live,
            accept_thread: Some(accept_thread),
        })
    }

    pub(super) fn status(&self) -> PublishedPortStatus {
        PublishedPortStatus::active(&self.spec, &self.workspace_ip)
    }

    pub(super) fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // A connection thread blocks in `read`, so shutting its sockets down is
        // what lets it notice the withdrawal. Without this the thread would stay
        // parked until its peer closed, holding its connection slot.
        self.connections.shutdown_all();
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ActivePublication {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone, Copy)]
pub(super) enum PublishMode {
    Strict,
    BestEffort,
}

pub(super) fn shutdown_publications(mut publications: Vec<ActivePublication>) {
    for publication in &mut publications {
        publication.shutdown();
    }
}
