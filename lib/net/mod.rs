use std::{
    collections::{HashMap, HashSet, hash_map},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use fallible_iterator::FallibleIterator;
use futures::{StreamExt, channel::mpsc};
use heed::types::{SerdeBincode, Unit};
use hickory_resolver::TokioResolver;
use parking_lot::RwLock;
use quinn::{ClientConfig, Endpoint, ServerConfig};
use sneed::{
    DatabaseUnique, EnvError, RoTxn, RwTxn, RwTxnError, UnitKey,
    db::error::Error as DbError,
};
use tokio_stream::StreamNotifyClose;
use tracing::instrument;

use crate::{
    archive::Archive,
    authorization::BatchVerificationContext,
    state::State,
    types::{
        AuthorizedTransaction, Network, THIS_SIDECHAIN, VERSION, Version,
        net::{DEFAULT_PORT, ResolvedSeedAddress, SeedAddress},
    },
    util::ErrorChain,
};

pub mod error;
mod peer;

pub use error::Error;
pub(crate) use peer::error::mailbox::Error as PeerConnectionMailboxError;
use peer::{
    Connection, ConnectionContext as PeerConnectionCtxt,
    ConnectionHandle as PeerConnectionHandle,
};
pub use peer::{
    ConnectionError as PeerConnectionError, Info as PeerConnectionInfo,
    InternalMessage as PeerConnectionMessage, Peer, PeerConnectionStatus,
    PeerStateId, Request as PeerRequest, ResponseMessage as PeerResponse,
    message as peer_message,
};

/// Dummy certificate verifier that treats any certificate as valid.
/// NOTE, such verification is vulnerable to MITM attacks, but convenient for testing.
#[derive(Debug)]
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer,
        _intermediates: &[rustls::pki_types::CertificateDer],
        _server_name: &rustls::pki_types::ServerName,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
    {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error>
    {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider()
                .signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn configure_client() -> Result<ClientConfig, error::ConfigureClient> {
    let crypto_provider = Arc::new(rustls::crypto::ring::default_provider());
    let crypto = rustls::ClientConfig::builder_with_provider(crypto_provider)
        .with_safe_default_protocol_versions()
        .map_err(error::configure_client::Inner::Rustls)?
        .dangerous()
        .with_custom_certificate_verifier(SkipServerVerification::new())
        .with_no_client_auth();
    let client_config =
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?;
    Ok(ClientConfig::new(Arc::new(client_config)))
}

/// Returns default server configuration along with its certificate.
fn configure_server(
    mut server_names: HashSet<String>,
) -> Result<(ServerConfig, Vec<u8>), Error> {
    server_names.insert("localhost".to_owned());
    let cert_key =
        rcgen::generate_simple_self_signed(Vec::from_iter(server_names))?;
    let keypair_der = cert_key.signing_key.serialize_der();
    let priv_key = rustls::pki_types::PrivateKeyDer::Pkcs8(keypair_der.into());
    let cert_der = cert_key.cert.der().to_vec();
    let cert_chain = vec![cert_key.cert.into()];

    let mut server_config =
        ServerConfig::with_single_cert(cert_chain, priv_key)?;
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_concurrent_uni_streams(1_u8.into());

    Ok((server_config, cert_der))
}

/// Constructs a QUIC endpoint configured to listen for incoming connections on a certain address
/// and port.
///
/// ## Returns
///
/// - a stream of incoming QUIC connections
/// - server certificate serialized into DER format
pub fn make_server_endpoint(
    bind_addr: SocketAddr,
    server_names: HashSet<String>,
) -> Result<(Endpoint, Vec<u8>), Error> {
    let (server_config, server_cert) = configure_server(server_names)?;

    tracing::info!("creating server endpoint: binding to {bind_addr}",);

    let mut endpoint = Endpoint::server(server_config, bind_addr)?;
    let client_cfg = configure_client()?;
    endpoint.set_default_client_config(client_cfg);
    Ok((endpoint, server_cert))
}

// None indicates that the stream has ended
pub type PeerInfoRx =
    mpsc::UnboundedReceiver<(SocketAddr, Option<PeerConnectionInfo>)>;

const SIGNET_SEED_NODE_ADDRS: &[SeedAddress<&str>] = {
    const SIGNET_MINING_SERVER: SeedAddress<&str> = SeedAddress {
        host: url::Host::Ipv4(Ipv4Addr::new(172, 105, 148, 135)),
        port: DEFAULT_PORT,
    };
    // thunder.bip300.xyz
    const BIP300_XYZ: SeedAddress<&str> = SeedAddress {
        host: url::Host::Ipv4(Ipv4Addr::new(95, 217, 243, 12)),
        port: DEFAULT_PORT,
    };
    &[SIGNET_MINING_SERVER, BIP300_XYZ]
};

const BETANET_SEED_NODE_ADDRS: &[SeedAddress<&str>] = {
    const ECASH_EU_COM: SeedAddress<&str> = SeedAddress {
        host: url::Host::Domain("seed.beta.ecash.eu.com"),
        port: DEFAULT_PORT,
    };
    &[ECASH_EU_COM]
};

/// Add every seed IP address the network names that the database does not
/// hold. A datadir made before a seed existed would otherwise never learn it.
/// A seed host name stays out of the database, and resolves at each start.
fn add_seed_peers(
    known_peers: &DatabaseUnique<SerdeBincode<SocketAddr>, Unit>,
    rwtxn: &mut RwTxn,
    network: Network,
) -> Result<(), DbError> {
    for seed_node_addr in seed_node_addrs(network)
        .iter()
        .filter_map(SeedAddress::socket_addr)
    {
        if known_peers.try_get(rwtxn, &seed_node_addr)?.is_none() {
            tracing::debug!(seed_node = %seed_node_addr, "adding seed node");
            known_peers.put(rwtxn, &seed_node_addr, &())?;
        }
    }
    Ok(())
}

const fn seed_node_addrs(
    network: Network,
) -> &'static [SeedAddress<&'static str>] {
    match network {
        Network::Betanet => BETANET_SEED_NODE_ADDRS,
        Network::Regtest => &[],
        Network::Signet => SIGNET_SEED_NODE_ADDRS,
    }
}

pub async fn resolve_seed_address(
    dns_resolver: &TokioResolver,
    seed_addr: SeedAddress,
) -> Result<ResolvedSeedAddress, error::ResolveSeedAddress> {
    let domain = match seed_addr.host {
        url::Host::Ipv4(ipv4) => {
            return Ok(ResolvedSeedAddress::Static(SocketAddr::new(
                IpAddr::V4(ipv4),
                seed_addr.port,
            )));
        }
        url::Host::Ipv6(ipv6) => {
            return Ok(ResolvedSeedAddress::Static(SocketAddr::new(
                IpAddr::V6(ipv6),
                seed_addr.port,
            )));
        }
        url::Host::Domain(domain) => domain,
    };
    let mut addrs: Vec<_> = dns_resolver
        .lookup_ip(domain.as_str())
        .await
        .map_err(|err| error::ResolveSeedAddress::Net(Box::new(err)))?
        .into_iter()
        .filter(|addr| !addr.is_unspecified())
        .collect();
    let Some(last_addr) = addrs.pop() else {
        tracing::warn!(%domain, "the seed host name resolved to no address");
        return Err(error::ResolveSeedAddress::NoIpAddrs { domain });
    };
    addrs.reverse();
    Ok(ResolvedSeedAddress::Domain {
        domain,
        port: seed_addr.port,
        addrs: nonempty::NonEmpty {
            head: last_addr,
            tail: addrs,
        },
    })
}

/// Handle to the tasks that dial seed host names. Drop aborts the tasks.
#[repr(transparent)]
pub struct DialSeedsHandle(
    tokio_util::task::JoinMap<SeedAddress, Result<(), error::DialSeed>>,
);

// Keep track of peer state
// Exchange metadata
// Bulk download
// Propagation
//
// Initial block download
//
// 1. Download headers
// 2. Download blocks
// 3. Update the state
#[derive(Clone)]
pub struct Net {
    pub server: Endpoint,
    archive: Archive,
    pub(crate) batch_verification_ctxt: BatchVerificationContext,
    pub dns_resolver: Arc<TokioResolver>,
    magic_bytes: peer::message::MagicBytes,
    state: State,
    active_peers: Arc<RwLock<HashMap<SocketAddr, PeerConnectionHandle>>>,
    // None indicates that the stream has ended
    peer_info_tx:
        mpsc::UnboundedSender<(SocketAddr, Option<PeerConnectionInfo>)>,
    known_peers: DatabaseUnique<SerdeBincode<SocketAddr>, Unit>,
    _version: DatabaseUnique<UnitKey, SerdeBincode<Version>>,
}

impl Net {
    pub const NUM_DBS: u32 = 2;

    fn add_active_peer(
        &self,
        addr: SocketAddr,
        peer_connection_handle: PeerConnectionHandle,
        info_rx: mpsc::UnboundedReceiver<PeerConnectionInfo>,
    ) -> Result<(), error::AlreadyConnected> {
        tracing::trace!(%addr, "add active peer: starting");
        let mut active_peers_write = self.active_peers.write();
        match active_peers_write.entry(addr) {
            hash_map::Entry::Occupied(_) => {
                tracing::error!(%addr, "add active peer: already connected");
                return Err(error::AlreadyConnected(addr));
            }
            hash_map::Entry::Vacant(active_peer_entry) => {
                active_peer_entry.insert(peer_connection_handle);
            }
        }
        drop(active_peers_write);
        tokio::spawn({
            let info_rx = StreamNotifyClose::new(info_rx)
                .map(move |info| Ok((addr, info)));
            let peer_info_tx = self.peer_info_tx.clone();
            async move {
                if let Err(_send_err) = info_rx.forward(peer_info_tx).await {
                    // Channel closed, which is normal when the connection fails or is closed
                    tracing::debug!(%addr, "Peer connection info channel closed (connection may have failed)");
                }
            }
        });
        Ok(())
    }

    pub fn remove_active_peer(&self, addr: SocketAddr) {
        tracing::trace!(%addr, "remove active peer: starting");
        let mut active_peers_write = self.active_peers.write();
        if let Some(peer_connection) = active_peers_write.remove(&addr) {
            drop(peer_connection);
            tracing::info!(%addr, "remove active peer: disconnected");
        }
    }

    /// Apply the provided function to the peer connection handle,
    /// if it exists.
    pub fn try_with_active_peer_connection<F, T>(
        &self,
        addr: SocketAddr,
        f: F,
    ) -> Option<T>
    where
        F: FnMut(&PeerConnectionHandle) -> T,
    {
        let active_peers_read = self.active_peers.read();
        active_peers_read.get(&addr).map(f)
    }

    // TODO: This should have more context.
    // Last received message, connection state, etc.
    pub fn get_active_peers(&self) -> Vec<Peer> {
        self.active_peers
            .read()
            .iter()
            .map(|(addr, conn_handle)| Peer {
                address: *addr,
                status: conn_handle.connection_status(),
            })
            .collect()
    }

    #[instrument(skip_all, fields(addr), err(Debug))]
    pub fn connect_peer(
        &self,
        env: sneed::Env<heed::WithoutTls>,
        mut resolved_addr: ResolvedSeedAddress,
    ) -> Result<(), Error> {
        {
            let active_peers = self.active_peers.read();
            for ip_addr in resolved_addr.ip_addrs() {
                let addr = SocketAddr::new(ip_addr, resolved_addr.port());
                if active_peers.contains_key(&addr) {
                    tracing::error!("connect peer: already connected");
                    return Err(error::AlreadyConnected(addr).into());
                }
            }
        }
        let (addr, connecting) = loop {
            let addr = SocketAddr::new(
                resolved_addr.first_ip_addr(),
                resolved_addr.port(),
            );
            // This check happens within Quinn with a
            // generic "invalid remote address". We run the
            // same check, and provide a friendlier error
            // message.
            if addr.ip().is_unspecified() {
                return Err(Error::UnspecfiedPeerIP(addr.ip()));
            }
            let server_name = match resolved_addr.host() {
                url::Host::Domain(domain) => domain,
                url::Host::Ipv4(_) | url::Host::Ipv6(_) => "localhost",
            };
            match self.server.connect(addr, server_name) {
                Ok(connecting) => break (addr, connecting),
                Err(err @ quinn::ConnectError::InvalidRemoteAddress(_)) => {
                    let (_, Some(next_addr)) =
                        resolved_addr.pop_first_ip_addr()
                    else {
                        return Err(err.into());
                    };
                    resolved_addr = next_addr;
                }
                Err(err) => return Err(err.into()),
            }
        };
        // A host name resolves again at each start, so only an IP address
        // goes into the database.
        if let ResolvedSeedAddress::Static(static_addr) = resolved_addr {
            let mut rwtxn = env.write_txn().map_err(EnvError::from)?;
            self.known_peers
                .put(&mut rwtxn, &static_addr, &())
                .map_err(DbError::from)?;
            rwtxn.commit().map_err(RwTxnError::from)?;
        }
        let connection_ctxt = PeerConnectionCtxt {
            env,
            archive: self.archive.clone(),
            batch_verification_ctxt: self.batch_verification_ctxt,
            magic_bytes: self.magic_bytes,
            resolved_address: resolved_addr,
            state: self.state.clone(),
        };

        let (connection_handle, info_rx) =
            peer::connect(connecting, connection_ctxt);
        self.add_active_peer(addr, connection_handle, info_rx)?;
        Ok(())
    }

    /// Delete peer from known_peers DB.
    /// Connections to the peer are not terminated.
    pub fn forget_peer(
        &self,
        rwtxn: &mut RwTxn,
        addr: &SocketAddr,
    ) -> Result<bool, Error> {
        self.known_peers
            .delete(rwtxn, addr)
            .map_err(|err| DbError::from(err).into())
    }

    // TODO setup coinshift.bip300.xyz as the seed node
    pub const SEED_NODE_ADDRS: &[SocketAddr] = {
        const SIGNET_MINING_SERVER: SocketAddr = SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
            4000 + THIS_SIDECHAIN as u16,
        );
        // thunder.bip300.xyz
        const BIP300_XYZ: SocketAddr = SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
            4000 + THIS_SIDECHAIN as u16,
        );
        &[SIGNET_MINING_SERVER, BIP300_XYZ]
    };

    async fn dial_seed(
        &self,
        env: sneed::Env<heed::WithoutTls>,
        seed_addr: SeedAddress,
    ) -> Result<(), error::DialSeed> {
        tracing::trace!(%seed_addr, "dial seed host name");
        let resolved_addr =
            resolve_seed_address(&self.dns_resolver, seed_addr).await?;
        self.connect_peer(env, resolved_addr)
            .map_err(|err| error::DialSeed::Connect(Box::new(err)))
    }

    fn known_peer_addrs(
        &self,
        rotxn: &RoTxn,
    ) -> Result<Vec<SocketAddr>, DbError> {
        let peer_addrs = self.known_peers.iter_keys(rotxn)?.collect()?;
        Ok(peer_addrs)
    }

    fn is_active_peer(&self, addr: &SocketAddr) -> bool {
        self.active_peers.read().contains_key(addr)
    }

    /// Dial a peer that the database knows.
    /// Returns `true` if a connection started, and `false` if the peer is
    /// already connected.
    fn dial_known_peer(
        &self,
        env: sneed::Env<heed::WithoutTls>,
        peer_addr: SocketAddr,
    ) -> Result<bool, Error> {
        if self.is_active_peer(&peer_addr) {
            return Ok(false);
        }
        tracing::trace!(%peer_addr, "dial known peer");
        let () = self.connect_peer(env, peer_addr.into())?;
        Ok(true)
    }

    /// Dial every peer that the database knows, seeds included.
    /// Returns the number of connections that started.
    fn dial_known_peers(
        &self,
        env: &sneed::Env<heed::WithoutTls>,
    ) -> Result<usize, Error> {
        let peer_addrs = {
            let rotxn = env.read_txn().map_err(EnvError::from)?;
            self.known_peer_addrs(&rotxn)?
        };
        let mut dialed = 0;
        for peer_addr in peer_addrs {
            match self.dial_known_peer(env.clone(), peer_addr) {
                Ok(true) => dialed += 1,
                Ok(false) => (),
                Err(err) => {
                    tracing::error!(%peer_addr, message = %ErrorChain::new(&err))
                }
            }
        }
        Ok(dialed)
    }

    /// Dial the known peers again while no peer connection exists.
    /// `min_delay` is the shortest wait between two checks for a connection.
    /// The wait doubles after each redial, up to `max_delay`.
    /// The future returns only on a database error.
    pub async fn redial_known_peers(
        &self,
        env: sneed::Env<heed::WithoutTls>,
        min_delay: Duration,
        max_delay: Duration,
    ) -> Result<(), Error> {
        let mut delay = min_delay;
        let mut no_peers_at_last_check = false;
        loop {
            tokio::time::sleep(delay).await;
            let active_peer_count = self.active_peers.read().len();
            if active_peer_count != 0 {
                delay = min_delay;
                no_peers_at_last_check = false;
                continue;
            }
            // The net task reconnects to a peer that errored. A redial waits
            // for a full delay with no connection, so it never dials first.
            if !no_peers_at_last_check {
                no_peers_at_last_check = true;
                continue;
            }
            let dialed = self.dial_known_peers(&env)?;
            tracing::info!(dialed, "no peer connection: dialed known peers");
            delay = (2 * delay).min(max_delay);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        runtime: &tokio::runtime::Handle,
        env: &sneed::Env<heed::WithoutTls>,
        archive: Archive,
        batch_verification_ctxt: BatchVerificationContext,
        magic_bytes_override: Option<peer::message::MagicBytes>,
        network: Network,
        state: State,
        bind_addr: SocketAddr,
        add_peers: HashSet<SeedAddress>,
        server_names: HashSet<String>,
    ) -> Result<(Self, PeerInfoRx, DialSeedsHandle), Error> {
        tracing::debug!(bind_addr = %bind_addr, "Net::new: Starting initialization");
        tracing::debug!("Net::new: Creating server endpoint");
        let (server, _) = make_server_endpoint(bind_addr, server_names)?;
        tracing::debug!("Net::new: Server endpoint created");
        let active_peers = Arc::new(RwLock::new(HashMap::new()));
        tracing::debug!("Net::new: Opening database transaction");
        let mut rwtxn = env.write_txn()?;
        tracing::debug!("Net::new: Opening/creating known_peers database");
        let known_peers =
            match DatabaseUnique::open(env, &rwtxn, "known_peers")? {
                Some(known_peers) => {
                    tracing::debug!(
                        "Net::new: Found existing known_peers database"
                    );
                    known_peers
                }
                None => {
                    tracing::debug!(
                        "Net::new: Creating new known_peers database"
                    );
                    DatabaseUnique::create(env, &mut rwtxn, "known_peers")?
                }
            };
        let () = add_seed_peers(&known_peers, &mut rwtxn, network)?;
        for peer_addr in add_peers.iter().filter_map(SeedAddress::socket_addr) {
            known_peers.put(&mut rwtxn, &peer_addr, &())?;
        }
        tracing::debug!("Net::new: Creating net_version database");
        let version = DatabaseUnique::create(env, &mut rwtxn, "net_version")?;
        if version.try_get(&rwtxn, &())?.is_none() {
            version.put(&mut rwtxn, &(), &*VERSION)?;
        }
        tracing::debug!("Net::new: Committing database transaction");
        rwtxn.commit().map_err(RwTxnError::from)?;
        tracing::debug!("Net::new: Creating peer info channel");
        let magic_bytes = magic_bytes_override
            .unwrap_or_else(|| peer::message::magic_bytes(network));
        let dns_resolver = {
            let builder = hickory_resolver::Resolver::builder_tokio()
                .map_err(Error::BuildDnsResolver)?;
            let resolver = builder.build().map_err(Error::BuildDnsResolver)?;
            Arc::new(resolver)
        };
        let (peer_info_tx, peer_info_rx) = mpsc::unbounded();
        let net = Net {
            server,
            archive,
            batch_verification_ctxt,
            dns_resolver,
            magic_bytes,
            state,
            active_peers,
            peer_info_tx,
            known_peers,
            _version: version,
        };
        tracing::debug!("Net::new: Reading known peers from database");
        let known_peers = {
            let rotxn = env.read_txn().map_err(EnvError::from)?;
            net.known_peer_addrs(&rotxn)?
        };
        tracing::info!(
            peer_count = known_peers.len(),
            "Net::new: Connecting to known peers"
        );
        let connection_start = std::time::Instant::now();
        // Attempt to connect to all known peers, but don't fail if some connections fail
        for peer_addr in known_peers {
            tracing::debug!(
                peer_addr = %peer_addr,
                "Net::new: Attempting to connect to known peer"
            );
            match net.connect_peer(env.clone(), peer_addr.into()) {
                Ok(()) => {
                    tracing::debug!(peer_addr = %peer_addr, "Net::new: Successfully initiated connection to peer");
                }
                Err(Error::Connect(
                    quinn::ConnectError::InvalidRemoteAddress(addr),
                )) => {
                    tracing::warn!(
                        %addr, "Net::new: Known peer with invalid remote address, removing"
                    );
                    let mut rwtxn = env.write_txn()?;
                    net.known_peers
                        .delete(&mut rwtxn, &peer_addr)
                        .map_err(DbError::from)?;
                    rwtxn.commit()?;
                    tracing::info!(
                        %addr,
                        "Net::new: Removed known peer with invalid remote address"
                    );
                }
                Err(err) => {
                    tracing::debug!(
                        peer_addr = %peer_addr,
                        error = %err,
                        "Net::new: Failed to connect to peer (will continue with other peers)"
                    );
                }
            }
        }
        let connection_elapsed = connection_start.elapsed();
        tracing::info!(
            elapsed_secs = connection_elapsed.as_secs_f64(),
            "Net::new: Finished connecting to known peers"
        );
        let seed_names: HashSet<SeedAddress> = seed_node_addrs(network)
            .iter()
            .map(|seed_addr| seed_addr.to_owned())
            .chain(add_peers)
            .filter(|seed_addr| seed_addr.socket_addr().is_none())
            .collect();
        let mut dial_seeds = tokio_util::task::JoinMap::new();
        for seed_addr in seed_names {
            let env = env.clone();
            let net = net.clone();
            dial_seeds.spawn_on(
                seed_addr.clone(),
                async move {
                    net.dial_seed(env, seed_addr).await.inspect_err(
                        |err| tracing::error!(message = %ErrorChain::new(err)),
                    )
                },
                runtime,
            );
        }
        tracing::info!("Net::new: Initialization complete");
        Ok((net, peer_info_rx, DialSeedsHandle(dial_seeds)))
    }

    /// Accept the next incoming connection. Returns Some(addr) if a connection was accepted
    /// and a new peer was added.
    pub async fn accept_incoming(
        &self,
        env: sneed::Env<heed::WithoutTls>,
    ) -> Result<Option<SocketAddr>, error::AcceptConnection> {
        tracing::debug!(
            "accept incoming: listening for connections on `{}`",
            self.server
                .local_addr()
                .map(|socket| socket.to_string())
                .unwrap_or("unknown address".into())
        );
        let connection = match self.server.accept().await {
            Some(conn) => {
                let remote_address = conn.remote_address();
                tracing::trace!("accepting connection from {remote_address}",);

                let raw_conn = conn.await.map_err(|error| {
                    error::AcceptConnection::Connection {
                        error,
                        remote_address,
                    }
                })?;
                Connection::new(raw_conn, self.magic_bytes)
            }
            None => {
                tracing::debug!("server endpoint closed");
                return Err(error::AcceptConnection::ServerEndpointClosed);
            }
        };
        let addr = connection.addr();

        tracing::trace!(%addr, "accepted incoming connection");
        if self.active_peers.read().contains_key(&addr) {
            tracing::info!(
                %addr, "incoming connection: already peered, refusing duplicate",
            );
            connection
                .inner
                .close(quinn::VarInt::from_u32(1), b"already connected");
        }
        if connection.inner.close_reason().is_some() {
            return Ok(None);
        }
        tracing::info!(%addr, "connected to new peer");
        let mut rwtxn = env.write_txn().map_err(EnvError::from)?;
        self.known_peers
            .put(&mut rwtxn, &addr, &())
            .map_err(DbError::from)?;
        rwtxn.commit().map_err(RwTxnError::from)?;

        tracing::trace!(%addr, "wrote peer to database");
        let connection_ctxt = PeerConnectionCtxt {
            env,
            archive: self.archive.clone(),
            batch_verification_ctxt: self.batch_verification_ctxt,
            magic_bytes: self.magic_bytes,
            resolved_address: addr.into(),
            state: self.state.clone(),
        };
        let (connection_handle, info_rx) =
            peer::handle(connection_ctxt, connection);
        self.add_active_peer(addr, connection_handle, info_rx)?;
        Ok(Some(addr))
    }

    /// Attempt to push an internal message to the specified peer
    /// Returns `true` if successful
    pub fn push_internal_message(
        &self,
        message: PeerConnectionMessage,
        addr: SocketAddr,
    ) -> bool {
        let active_peers_read = self.active_peers.read();
        let Some(peer_connection_handle) = active_peers_read.get(&addr) else {
            let err = Error::MissingPeerConnection(addr);
            tracing::warn!("{:#}", anyhow::Error::from(err));
            return false;
        };

        if let Err(send_err) = peer_connection_handle
            .internal_message_tx
            .unbounded_send(message)
        {
            let message = send_err.into_inner();
            tracing::warn!(
                "Failed to push internal message to peer connection {addr}: {message:?}"
            );
            return false;
        }
        true
    }

    /// Push a tx to all active peers, except those in the provided set
    pub fn push_tx(
        &self,
        exclude: HashSet<SocketAddr>,
        tx: AuthorizedTransaction,
    ) {
        self.active_peers
            .read()
            .iter()
            .filter(|(addr, _)| !exclude.contains(addr))
            .for_each(|(addr, peer_connection_handle)| {
                match peer_connection_handle.connection_status() {
                    PeerConnectionStatus::Connecting => {
                        tracing::trace!(%addr, "skipping peer at {addr} because it is not fully connected");
                        return;
                    }
                    PeerConnectionStatus::Connected => {}
                }
                let request: PeerRequest = peer::message::PushTransactionRequest {
                    transaction: tx.clone(),
                }.into();
                if let Err(_send_err) = peer_connection_handle
                    .internal_message_tx
                    .unbounded_send(request.into())
                {
                    let txid = tx.transaction.txid();
                    tracing::warn!("Failed to push tx {txid} to peer at {addr}")
                }
            })
    }
}

#[cfg(test)]
mod crypto_provider_tests {
    use super::{configure_client, configure_server};

    /// Building the QUIC TLS configs must not panic.
    ///
    /// Both build their rustls config from the *process-level* provider. When
    /// the build graph offers more than one — which it does whenever
    /// `integration_tests` is compiled, because `bdk_electrum` pulls in
    /// `aws-lc-rs` alongside our `ring` — rustls refuses to choose and panics
    /// here, at node startup, from a workspace that built without a single
    /// warning.
    ///
    /// [`crate::install_default_crypto_provider`] is what settles it, and every
    /// binary calls it first thing in `main`. This test pins that arrangement:
    /// drop the call and this panics, rather than the node doing it in front of
    /// an operator a release later.
    ///
    /// The endpoint itself is not built here — `quinn::Endpoint::server` wants
    /// a Tokio reactor, and everything this is about happens before that.
    #[test]
    fn tls_configs_build_once_a_provider_is_installed() {
        crate::install_default_crypto_provider();
        let (_server_config, cert) = configure_server(Default::default())
            .expect("server config should build");
        assert!(!cert.is_empty(), "server config should yield a certificate");
        let _client_config =
            configure_client().expect("client config should build");
    }
}

#[cfg(test)]
mod seed_peer_tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    use heed::types::{SerdeBincode, Unit};
    use sneed::DatabaseUnique;

    use super::{add_seed_peers, resolve_seed_address, seed_node_addrs};
    use crate::types::{Network, net::SeedAddress};

    /// Every seed reaches a peer table that already exists, and a second call
    /// writes the same set.
    #[test]
    fn seeds_reach_an_existing_database() -> anyhow::Result<()> {
        let temp_dir = temp_dir::TempDir::new()?;
        let mut opts = heed::EnvOpenOptions::new().read_txn_without_tls();
        opts.map_size(16 * 1024 * 1024).max_dbs(2);
        let env = unsafe { sneed::Env::open(&opts, temp_dir.path()) }?;
        let network = Network::Signet;
        let known_peers = {
            let mut rwtxn = env.write_txn()?;
            let known_peers: DatabaseUnique<SerdeBincode<SocketAddr>, Unit> =
                DatabaseUnique::create(&env, &mut rwtxn, "known_peers")?;
            add_seed_peers(&known_peers, &mut rwtxn, network)?;
            add_seed_peers(&known_peers, &mut rwtxn, network)?;
            rwtxn.commit()?;
            known_peers
        };
        let rotxn = env.read_txn()?;
        let seed_socket_addrs: Vec<SocketAddr> = seed_node_addrs(network)
            .iter()
            .filter_map(SeedAddress::socket_addr)
            .collect();
        for seed_node_addr in &seed_socket_addrs {
            anyhow::ensure!(
                known_peers.try_get(&rotxn, seed_node_addr)?.is_some(),
                "the seed {seed_node_addr} never reached the database"
            );
        }
        assert_eq!(known_peers.len(&rotxn)?, seed_socket_addrs.len() as u64);
        Ok(())
    }

    /// A seed names a host and a port, and the resolver keeps both.
    #[tokio::test]
    async fn a_seed_name_resolves_with_its_port() -> anyhow::Result<()> {
        let dns_resolver =
            hickory_resolver::Resolver::builder_tokio()?.build()?;
        let seed_addr: SeedAddress = "localhost:4004".parse()?;
        let resolved = resolve_seed_address(&dns_resolver, seed_addr).await?;
        assert_eq!(resolved.port(), 4004);
        assert!(
            resolved
                .ip_addrs()
                .any(|addr| addr == IpAddr::V4(Ipv4Addr::LOCALHOST)
                    || addr == IpAddr::V6(Ipv6Addr::LOCALHOST))
        );
        Ok(())
    }
}

#[cfg(test)]
mod peer_handle_test {
    use super::{
        DialSeedsHandle, Net, PeerConnectionCtxt, PeerConnectionInfo,
        PeerInfoRx, make_server_endpoint, peer,
    };
    use crate::{
        archive::Archive,
        state::State,
        types::{
            Network,
            net::{ResolvedSeedAddress, SeedAddress},
        },
    };
    use anyhow::Context;
    use futures::StreamExt;
    use std::{
        collections::HashSet,
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        time::Duration,
    };

    fn temp_net(
        test_name: &str,
    ) -> anyhow::Result<(
        temp_dir::TempDir,
        sneed::Env<heed::WithoutTls>,
        Net,
        PeerInfoRx,
    )> {
        let (temp_dir, env, net, info_rx, _dial_seeds) =
            temp_net_with_peers(test_name, HashSet::new())?;
        Ok((temp_dir, env, net, info_rx))
    }

    fn temp_net_with_peers(
        test_name: &str,
        add_peers: HashSet<SeedAddress>,
    ) -> anyhow::Result<(
        temp_dir::TempDir,
        sneed::Env<heed::WithoutTls>,
        Net,
        PeerInfoRx,
        DialSeedsHandle,
    )> {
        let temp_dir =
            temp_dir::TempDir::with_prefix(format!("coinshift-{test_name}-"))?;
        let mut opts = heed::EnvOpenOptions::new().read_txn_without_tls();
        opts.map_size(16 * 1024 * 1024)
            .max_dbs(Archive::NUM_DBS + State::NUM_DBS + Net::NUM_DBS);
        let env = unsafe { sneed::Env::open(&opts, temp_dir.path()) }?;
        let archive = Archive::new(&env)?;
        let state = State::new(&env)?;
        let (net, info_rx, dial_seeds) = Net::new(
            &tokio::runtime::Handle::current(),
            &env,
            archive,
            crate::authorization::BatchVerificationContext::new(
                &mut rand::rng(),
            ),
            None,
            Network::Regtest,
            state,
            (Ipv4Addr::LOCALHOST, 0).into(),
            add_peers,
            HashSet::new(),
        )?;
        Ok((temp_dir, env, net, info_rx, dial_seeds))
    }

    #[tokio::test]
    async fn connect_peer_skips_ipv6_on_an_ipv4_endpoint() -> anyhow::Result<()>
    {
        let (_temp_dir, env, net, mut info_rx) = temp_net("peer-family")?;
        let (remote, _) = make_server_endpoint(
            (Ipv4Addr::LOCALHOST, 0).into(),
            HashSet::new(),
        )?;
        let addr = remote.local_addr()?;
        let next_ip = Ipv4Addr::new(127, 0, 0, 2);
        let resolved = ResolvedSeedAddress::Domain {
            domain: "localhost".to_owned(),
            port: addr.port(),
            addrs: nonempty::NonEmpty {
                head: next_ip.into(),
                tail: vec![
                    Ipv4Addr::LOCALHOST.into(),
                    Ipv6Addr::LOCALHOST.into(),
                ],
            },
        };

        net.connect_peer(env, resolved)?;

        let peers = net.get_active_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].address, addr);
        assert!(net.server.local_addr()?.is_ipv4());
        net.server.close(0_u32.into(), b"test complete");
        let (reported_addr, info) =
            tokio::time::timeout(Duration::from_secs(5), info_rx.next())
                .await?
                .context("the peer task returned no result")?;
        let Some(PeerConnectionInfo::Error { resolved_addr, .. }) = info else {
            anyhow::bail!("the peer task returned no connection error");
        };
        assert_eq!(reported_addr, addr);
        assert_eq!(
            resolved_addr.ip_addrs().collect::<Vec<_>>(),
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST), next_ip.into()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn connect_peer_returns_the_last_invalid_address()
    -> anyhow::Result<()> {
        let (_temp_dir, env, net, _info_rx) = temp_net("peer-ipv6")?;
        let addr = SocketAddr::from((Ipv6Addr::LOCALHOST, 4004));
        let resolved = ResolvedSeedAddress::Domain {
            domain: "localhost".to_owned(),
            port: addr.port(),
            addrs: nonempty::NonEmpty {
                head: addr.ip(),
                tail: vec!["::2".parse()?],
            },
        };

        let error = net.connect_peer(env, resolved).unwrap_err();

        assert!(matches!(
            error,
            super::Error::Connect(
                quinn::ConnectError::InvalidRemoteAddress(failed)
            ) if failed == addr
        ));
        assert!(net.get_active_peers().is_empty());
        Ok(())
    }

    /// A seed host name resolves at startup, the node dials it, and the
    /// database holds no resolved address for it.
    #[tokio::test]
    async fn a_seed_host_name_dials_at_startup() -> anyhow::Result<()> {
        let (remote, _) = make_server_endpoint(
            (Ipv4Addr::LOCALHOST, 0).into(),
            HashSet::new(),
        )?;
        let addr = remote.local_addr()?;
        let seed_addr: SeedAddress =
            format!("localhost:{}", addr.port()).parse()?;
        let (_temp_dir, env, net, _info_rx, _dial_seeds) =
            temp_net_with_peers("seed-name", HashSet::from([seed_addr]))?;

        tokio::time::timeout(Duration::from_secs(5), async {
            while net.get_active_peers().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("the node did not dial the seed host name")?;

        let peers = net.get_active_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].address, addr);
        let rotxn = env.read_txn()?;
        assert_eq!(net.known_peers.len(&rotxn)?, 0);
        Ok(())
    }

    #[tokio::test]
    async fn connect_peer_keeps_a_static_ipv4_address() -> anyhow::Result<()> {
        let (_temp_dir, env, net, _info_rx) = temp_net("peer-static")?;
        let (remote, _) = make_server_endpoint(
            (Ipv4Addr::LOCALHOST, 0).into(),
            HashSet::new(),
        )?;
        let addr = remote.local_addr()?;

        net.connect_peer(env, addr.into())?;

        let peers = net.get_active_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].address, addr);
        Ok(())
    }

    #[tokio::test]
    async fn connect_peer_returns_other_quinn_errors() -> anyhow::Result<()> {
        let (_temp_dir, env, net, _info_rx) = temp_net("peer-closed")?;
        net.server.close(0_u32.into(), b"test complete");
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 4004));

        let error = net.connect_peer(env, addr.into()).unwrap_err();

        assert!(matches!(
            error,
            super::Error::Connect(quinn::ConnectError::EndpointStopping)
        ));
        assert!(net.get_active_peers().is_empty());
        Ok(())
    }

    const TEST_REDIAL_MIN_DELAY: Duration = Duration::from_millis(50);
    const TEST_REDIAL_MAX_DELAY: Duration = Duration::from_millis(200);

    /// A peer that drops leaves no connection, so the node dials it again.
    #[tokio::test]
    async fn a_lost_peer_is_dialed_again() -> anyhow::Result<()> {
        let (_temp_dir, env, net, _info_rx) = temp_net("peer-redial")?;
        let (remote, _) = make_server_endpoint(
            (Ipv4Addr::LOCALHOST, 0).into(),
            HashSet::new(),
        )?;
        let addr = remote.local_addr()?;
        net.connect_peer(env.clone(), addr.into())?;
        assert_eq!(net.get_active_peers().len(), 1);
        net.remove_active_peer(addr);
        assert!(net.get_active_peers().is_empty());
        let redial = tokio::spawn({
            let env = env.clone();
            let net = net.clone();
            async move {
                net.redial_known_peers(
                    env,
                    TEST_REDIAL_MIN_DELAY,
                    TEST_REDIAL_MAX_DELAY,
                )
                .await
            }
        });

        let dialed_again =
            tokio::time::timeout(Duration::from_secs(5), async {
                while net.get_active_peers().is_empty() {
                    tokio::time::sleep(TEST_REDIAL_MIN_DELAY).await;
                }
            })
            .await;

        redial.abort();
        dialed_again.context("the node dialed the lost peer no more")?;
        assert_eq!(net.get_active_peers()[0].address, addr);
        Ok(())
    }

    /// A peer that holds a connection takes no redial, and the loop starts no
    /// second connection to it.
    #[tokio::test]
    async fn a_connected_peer_takes_no_redial() -> anyhow::Result<()> {
        let (_temp_dir, env, net, _info_rx) = temp_net("peer-redial-skip")?;
        let (remote, _) = make_server_endpoint(
            (Ipv4Addr::LOCALHOST, 0).into(),
            HashSet::new(),
        )?;
        let addr = remote.local_addr()?;
        net.connect_peer(env.clone(), addr.into())?;

        assert!(!net.dial_known_peer(env.clone(), addr)?);
        assert_eq!(net.dial_known_peers(&env)?, 0);

        let redial = tokio::spawn({
            let env = env.clone();
            let net = net.clone();
            async move {
                net.redial_known_peers(
                    env,
                    TEST_REDIAL_MIN_DELAY,
                    TEST_REDIAL_MAX_DELAY,
                )
                .await
            }
        });
        tokio::time::sleep(TEST_REDIAL_MAX_DELAY * 5).await;
        redial.abort();
        let peers = net.get_active_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].address, addr);
        Ok(())
    }

    #[tokio::test]
    async fn rejected_duplicate_has_no_peer_close_event() -> anyhow::Result<()>
    {
        let (_temp_dir, env, net, info_rx) = temp_net("peer-duplicate")?;
        let (remote, _) = make_server_endpoint(
            (Ipv4Addr::LOCALHOST, 0).into(),
            HashSet::new(),
        )?;
        let addr = remote.local_addr()?;
        net.connect_peer(env.clone(), addr.into())?;
        let connection_ctxt = PeerConnectionCtxt {
            env,
            archive: net.archive.clone(),
            batch_verification_ctxt: net.batch_verification_ctxt,
            magic_bytes: net.magic_bytes,
            resolved_address: addr.into(),
            state: net.state.clone(),
        };
        let (duplicate, duplicate_info) = peer::connect(
            net.server.connect(addr, "localhost")?,
            connection_ctxt,
        );
        let error = net
            .add_active_peer(addr, duplicate, duplicate_info)
            .unwrap_err();
        assert_eq!(error.0, addr);
        assert_eq!(net.get_active_peers().len(), 1);
        drop(net);
        let events = tokio::time::timeout(
            Duration::from_secs(5),
            info_rx.collect::<Vec<_>>(),
        )
        .await?;
        assert_eq!(events.iter().filter(|(_, info)| info.is_none()).count(), 1);
        Ok(())
    }
}
