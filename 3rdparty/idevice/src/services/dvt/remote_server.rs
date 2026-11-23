//! Remote Server Client implementation for iOS instruments protocol.
//!
//! This module provides a client for communicating with iOS devices through the
//! remote server protocol used by instruments. It handles channel management and
//! message passing between the host and device.
//!
//! Remote Server communicates via NSKeyedArchives. These archives are binary plists
//! formatted specifically for naive recreation at the target.
//! Requests are sent as method calls to objective C objects on the device.
//!
//! # Overview
//! The client manages multiple communication channels and provides methods for:
//! - Creating new channels
//! - Sending method calls
//! - Reading responses
//!
//! # Example
//! ```rust,no_run
//! use std::sync::Arc;
//! use tokio::net::TcpStream;
//! use your_crate::{ReadWrite, IdeviceError};
//! use your_crate::instruments::RemoteServerClient;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), IdeviceError> {
//!     // Establish connection to device over the tunnel (see XPC docs)
//!     let transport = TcpStream::connect("1.2.3.4:1234").await?;
//!
//!     // Create client
//!     let mut client = RemoteServerClient::new(transport);
//!
//!     // Read the first message
//!     client.read_message(0).await?;
//!
//!     // Call a method on root channel
//!     client.call_method(
//!         0,
//!         Some("someMethod"),
//!         Some(vec![AuxValue::String("param".into())]),
//!         true
//!     ).await?;
//!
//!     // Read response
//!     let response = client.read_message(0).await?;
//!     println!("Got response: {:?}", response);
//!
//!
//!     Ok(())
//! }
//! ```

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

#[cfg(not(feature = "xctest"))]
use std::io;

use plist::Dictionary;
use tokio::{
    io::{AsyncWriteExt, ReadHalf, WriteHalf},
    sync::{Mutex, Notify, oneshot},
    task::JoinHandle,
};
use tracing::{debug, warn};

use super::errors::DvtError;

#[cfg(feature = "xctest")]
fn remote_timeout_error(timeout: std::time::Duration) -> IdeviceError {
    IdeviceError::XcTestTimeout(timeout.as_secs_f64())
}

#[cfg(not(feature = "xctest"))]
fn remote_timeout_error(timeout: std::time::Duration) -> IdeviceError {
    IdeviceError::Socket(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "remote server operation timed out after {:.1}s",
            timeout.as_secs_f64()
        ),
    ))
}

use crate::{
    IdeviceError, ReadWrite,
    dvt::message::{Aux, AuxValue, Message, MessageHeader, PayloadHeader},
};

/// Message type identifier for instruments protocol
pub const INSTRUMENTS_MESSAGE_TYPE: u32 = 2;

/// Client for communicating with iOS remote server protocol
///
/// Manages multiple communication channels and handles message serialization/deserialization.
/// Each channel operates independently and maintains its own message queue.
pub struct RemoteServerClient<R: ReadWrite> {
    label: Arc<str>,
    shared: Arc<RemoteServerShared<WriteHalf<R>>>,
    reader_task: JoinHandle<()>,
}

/// Handle to a specific communication channel
///
/// Provides channel-specific operations for use on the remote server client.
#[derive(Debug)]
pub struct Channel<'a, R: ReadWrite> {
    /// Reference to parent client
    client: &'a mut RemoteServerClient<R>,
    /// Channel number this handle operates on
    channel: i32,
}

/// Owned handle to a specific communication channel.
///
/// This mirrors pymobiledevice3's `DTXChannel` lifetime model more closely
/// than the borrowed [`Channel`]: it keeps only the shared transport state and
/// the channel code, so service/proxy wrappers can outlive a temporary
/// `&mut RemoteServerClient` borrow.
#[derive(Debug)]
pub struct OwnedChannel<R: ReadWrite> {
    label: Arc<str>,
    shared: Arc<RemoteServerShared<WriteHalf<R>>>,
    channel: i32,
}

impl<R: ReadWrite> Clone for OwnedChannel<R> {
    fn clone(&self) -> Self {
        Self {
            label: self.label.clone(),
            shared: self.shared.clone(),
            channel: self.channel,
        }
    }
}

type IncomingMessageHandler = Arc<
    dyn Fn(
            Message,
        )
            -> Pin<Box<dyn Future<Output = Result<IncomingHandlerOutcome, IdeviceError>> + Send>>
        + Send
        + Sync,
>;

type IncomingChannelInitializer<W> = Arc<
    dyn Fn(
            Arc<str>,
            Arc<RemoteServerShared<W>>,
            i32,
            String,
        ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send>>
        + Send
        + Sync,
>;

pub(crate) enum IncomingHandlerOutcome {
    Unhandled,
    HandledNoReply,
    Reply(Vec<u8>),
}

#[derive(Debug, Default)]
struct ChannelQueue {
    messages: Mutex<VecDeque<Message>>,
    notify: Notify,
}

#[derive(Debug, Clone)]
struct ChannelMetadata {
    code: i32,
    identifier: String,
    remote: bool,
}

struct IncomingChannelRegistration<W> {
    identifiers: Vec<String>,
    initializer: IncomingChannelInitializer<W>,
}

#[derive(Debug, Clone)]
enum CapabilityHandshakeState {
    Pending,
    Skipped,
    Received(Dictionary),
}

struct RemoteServerShared<W> {
    label: Arc<str>,
    writer: Mutex<W>,
    current_message: AtomicU32,
    new_channel: AtomicU32,
    channels: Mutex<HashMap<i32, Arc<ChannelQueue>>>,
    channel_metadata: Mutex<HashMap<i32, ChannelMetadata>>,
    pending_replies: Mutex<HashMap<u32, oneshot::Sender<Message>>>,
    handlers: Mutex<HashMap<i32, IncomingMessageHandler>>,
    incoming_channel_registrations: Mutex<Vec<IncomingChannelRegistration<W>>>,
    registry_notify: Notify,
    supported_identifiers: Mutex<CapabilityHandshakeState>,
    handshake_notify: Notify,
    closed: AtomicBool,
    closed_notify: Notify,
}

impl<W> std::fmt::Debug for RemoteServerShared<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteServerShared")
            .field(
                "current_message",
                &self.current_message.load(Ordering::Relaxed),
            )
            .field("new_channel", &self.new_channel.load(Ordering::Relaxed))
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<W> RemoteServerShared<W> {
    fn new(label: Arc<str>, writer: W) -> Self {
        let mut channels = HashMap::new();
        channels.insert(0, Arc::new(ChannelQueue::default()));
        let mut channel_metadata = HashMap::new();
        channel_metadata.insert(
            0,
            ChannelMetadata {
                code: 0,
                identifier: "ctrl".into(),
                remote: false,
            },
        );
        Self {
            label,
            writer: Mutex::new(writer),
            current_message: AtomicU32::new(0),
            new_channel: AtomicU32::new(1),
            channels: Mutex::new(channels),
            channel_metadata: Mutex::new(channel_metadata),
            pending_replies: Mutex::new(HashMap::new()),
            handlers: Mutex::new(HashMap::new()),
            incoming_channel_registrations: Mutex::new(Vec::new()),
            registry_notify: Notify::new(),
            supported_identifiers: Mutex::new(CapabilityHandshakeState::Pending),
            handshake_notify: Notify::new(),
            closed: AtomicBool::new(false),
            closed_notify: Notify::new(),
        }
    }
}

impl<R: ReadWrite> std::fmt::Debug for RemoteServerClient<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteServerClient")
            .field("shared", &"<remote-server-shared>")
            .finish()
    }
}

impl<R: ReadWrite> RemoteServerClient<R> {
    /// Creates a new client with a debug label used in tracing output.
    fn with_label_typed(idevice: R, label: impl Into<String>) -> Self
    where
        R: 'static,
    {
        let (reader, writer) = tokio::io::split(idevice);
        let label: Arc<str> = label.into().into();
        let shared = Arc::new(RemoteServerShared::new(label.clone(), writer));
        let reader_task = Self::spawn_reader(label.clone(), shared.clone(), reader);
        Self {
            label,
            shared,
            reader_task,
        }
    }

    /// Returns a handle to the root channel (channel 0)
    pub fn root_channel<'c>(&'c mut self) -> Channel<'c, R> {
        Channel {
            client: self,
            channel: 0,
        }
    }

    /// Returns a future that resolves when this DTX connection disconnects.
    ///
    /// This captures the shared state by clone so callers can await it
    /// alongside operations that hold a mutable borrow of the client.
    pub(crate) fn disconnect_waiter(&self) -> impl Future<Output = ()> + Send + 'static
    where
        R: 'static,
    {
        let shared = self.shared.clone();
        async move {
            if shared.closed.load(Ordering::Relaxed) {
                return;
            }
            shared.closed_notify.notified().await;
        }
    }

    /// Returns the peer capabilities received during `_notifyOfPublishedCapabilities:`.
    pub(crate) async fn supported_identifiers(&self) -> Option<Dictionary> {
        match &*self.shared.supported_identifiers.lock().await {
            CapabilityHandshakeState::Received(dict) => Some(dict.clone()),
            CapabilityHandshakeState::Pending | CapabilityHandshakeState::Skipped => None,
        }
    }

    /// Waits for `_notifyOfPublishedCapabilities:` from the remote side.
    pub(crate) async fn wait_for_capabilities(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Dictionary, IdeviceError> {
        tokio::time::timeout(timeout, async {
            loop {
                match &*self.shared.supported_identifiers.lock().await {
                    CapabilityHandshakeState::Received(dict) => return Ok(dict.clone()),
                    CapabilityHandshakeState::Skipped => {
                        return Err(IdeviceError::UnexpectedResponse(
                            "unexpected response".into(),
                        ));
                    }
                    CapabilityHandshakeState::Pending => {}
                }

                if self.shared.closed.load(Ordering::Relaxed) {
                    return Err(Self::closed_error());
                }

                tokio::select! {
                    _ = self.shared.handshake_notify.notified() => {}
                    _ = self.shared.closed_notify.notified() => return Err(Self::closed_error()),
                }
            }
        })
        .await
        .map_err(|_| remote_timeout_error(timeout))?
    }

    /// Performs the DTX capability handshake, mirroring pymobiledevice3's
    /// `DTXConnection._perform_handshake()`.
    pub(crate) async fn perform_handshake(
        &mut self,
        capabilities: Option<Dictionary>,
        timeout: std::time::Duration,
    ) -> Result<Option<Dictionary>, IdeviceError> {
        let already_received = self.supported_identifiers().await;

        {
            let mut state = self.shared.supported_identifiers.lock().await;
            *state = match (capabilities.is_some(), already_received.as_ref()) {
                (false, _) => CapabilityHandshakeState::Skipped,
                (true, Some(dict)) => CapabilityHandshakeState::Received(dict.clone()),
                (true, None) => CapabilityHandshakeState::Pending,
            };
        }

        if let Some(capabilities) = capabilities {
            self.root_channel()
                .call_method(
                    Some("_notifyOfPublishedCapabilities:"),
                    Some(vec![AuxValue::archived_value(plist::Value::Dictionary(
                        capabilities,
                    ))]),
                    false,
                )
                .await?;
        } else {
            return Ok(None);
        }

        if let Some(capabilities) = already_received {
            return Ok(Some(capabilities));
        }

        tokio::time::timeout(timeout, async {
            loop {
                match &*self.shared.supported_identifiers.lock().await {
                    CapabilityHandshakeState::Received(dict) => return Ok(Some(dict.clone())),
                    CapabilityHandshakeState::Skipped => return Ok(None),
                    CapabilityHandshakeState::Pending => {}
                }

                if self.shared.closed.load(Ordering::Relaxed) {
                    return Err(Self::closed_error());
                }

                tokio::select! {
                    _ = self.shared.handshake_notify.notified() => {}
                    _ = self.shared.closed_notify.notified() => return Err(Self::closed_error()),
                }
            }
        })
        .await
        .map_err(|_| remote_timeout_error(timeout))?
    }

    /// Creates a new channel with the given identifier
    ///
    /// # Arguments
    /// * `identifier` - String identifier for the new channel
    ///
    /// # Returns
    /// * `Ok(Channel)` - Handle to the new channel
    /// * `Err(IdeviceError)` - If channel creation fails
    ///
    /// # Errors
    /// * `IdeviceError::UnexpectedResponse("unexpected response".into()) if server responds with unexpected data
    /// * Other IO or serialization errors
    #[allow(unreachable_code)]
    pub async fn make_channel<'c>(
        &'c mut self,
        identifier: impl Into<String>,
    ) -> Result<Channel<'c, R>, IdeviceError> {
        let code = self.shared.new_channel.fetch_add(1, Ordering::Relaxed) as i32;
        let identifier = identifier.into();
        self.register_channel_metadata(code, identifier.clone(), false)
            .await;
        self.ensure_channel_registered(code).await;

        let args = vec![
            AuxValue::U32(
                code.try_into()
                    .expect("locally opened channels are positive"),
            ),
            AuxValue::Array(
                ns_keyed_archive::encode::encode_to_bytes(plist::Value::String(identifier))
                    .expect("Failed to encode"),
            ),
        ];

        let reply = self
            .call_method_with_reply(0, Some("_requestChannelWithCode:identifier:"), Some(args))
            .await?;

        if reply.data.is_some() {
            warn!("make_channel: unexpected reply payload: {:?}", reply.data);
            return Err(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ));
        }

        self.build_channel(code)
    }

    /// Opens a named service channel.
    ///
    /// This is a service-level alias for `make_channel()` that mirrors the
    /// terminology used by pymobiledevice3's `DTXConnection.open_channel()`.
    pub(crate) async fn open_service_channel<'c>(
        &'c mut self,
        identifier: &str,
    ) -> Result<Channel<'c, R>, IdeviceError> {
        self.make_channel(identifier).await
    }

    /// Opens a `dtxproxy:` channel assembled from local/remote service names.
    ///
    /// Mirrors pymobiledevice3's proxy-channel naming model, where the caller
    /// reasons about the two sub-services and the transport constructs the
    /// wire identifier.
    pub(crate) async fn make_proxy_channel<'c>(
        &'c mut self,
        local_service: &str,
        remote_service: &str,
    ) -> Result<Channel<'c, R>, IdeviceError> {
        self.make_channel(format!("dtxproxy:{local_service}:{remote_service}"))
            .await
    }

    /// Opens a proxied service channel assembled from local/remote service names.
    ///
    /// This is a service-level alias for `make_proxy_channel()` that matches
    /// the "proxy service" terminology used in pymobiledevice3.
    pub(crate) async fn open_proxied_service_channel<'c>(
        &'c mut self,
        local_service: &str,
        remote_service: &str,
    ) -> Result<Channel<'c, R>, IdeviceError> {
        self.make_proxy_channel(local_service, remote_service).await
    }

    fn build_channel<'c>(&'c mut self, code: i32) -> Result<Channel<'c, R>, IdeviceError> {
        Ok(Channel {
            client: self,
            channel: code,
        })
    }

    /// Returns an owned handle for an existing registered channel.
    pub(crate) fn accept_owned_channel(&self, code: i32) -> OwnedChannel<R> {
        OwnedChannel {
            label: self.label.clone(),
            shared: self.shared.clone(),
            channel: code,
        }
    }

    /// Registers an initializer that runs as soon as the remote opens a
    /// matching incoming channel via `_requestChannelWithCode:identifier:`.
    ///
    /// This mirrors pymobiledevice3's service instantiation timing more
    /// closely: the handler is installed before we acknowledge the channel
    /// request, so the channel can start handling inbound invokes
    /// immediately after the peer receives the OK reply.
    pub(crate) async fn register_incoming_channel_initializer<F, Fut>(
        &mut self,
        identifiers: &[&str],
        initializer: F,
    ) where
        F: Fn(OwnedChannel<R>, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), IdeviceError>> + Send + 'static,
    {
        let identifiers = identifiers
            .iter()
            .map(|identifier| (*identifier).to_owned())
            .collect();
        let initializer: IncomingChannelInitializer<WriteHalf<R>> =
            Arc::new(move |label, shared, channel, identifier| {
                let owned = OwnedChannel {
                    label,
                    shared,
                    channel,
                };
                Box::pin(initializer(owned, identifier))
            });
        self.shared
            .incoming_channel_registrations
            .lock()
            .await
            .push(IncomingChannelRegistration {
                identifiers,
                initializer,
            });
    }

    async fn register_channel_metadata(&self, code: i32, identifier: String, remote: bool) {
        self.shared.channel_metadata.lock().await.insert(
            code,
            ChannelMetadata {
                code,
                identifier,
                remote,
            },
        );
        self.shared.registry_notify.notify_waiters();
    }

    pub(crate) async fn wait_for_registered_channel_code(
        &self,
        identifiers: &[&str],
        remote: Option<bool>,
        timeout: Option<std::time::Duration>,
    ) -> Result<i32, IdeviceError> {
        let wait_future = async {
            loop {
                if let Some(code) = self.find_registered_channel_code(identifiers, remote).await {
                    return Ok(code);
                }

                if self.shared.closed.load(Ordering::Relaxed) {
                    return Err(Self::closed_error());
                }

                tokio::select! {
                    _ = self.shared.registry_notify.notified() => {}
                    _ = self.shared.closed_notify.notified() => return Err(Self::closed_error()),
                }
            }
        };

        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, wait_future)
                .await
                .map_err(|_| remote_timeout_error(timeout))?,
            None => wait_future.await,
        }
    }

    /// Waits for the code of a service channel matching one of the given identifiers.
    pub(crate) async fn wait_for_service_channel_code(
        &self,
        identifiers: &[&str],
        remote: Option<bool>,
        timeout: Option<std::time::Duration>,
    ) -> Result<i32, IdeviceError> {
        self.wait_for_registered_channel_code(identifiers, remote, timeout)
            .await
    }

    pub(crate) async fn wait_for_proxied_channel_code(
        &self,
        identifiers: &[&str],
        remote_service: bool,
        remote_channel: Option<bool>,
        timeout: Option<std::time::Duration>,
    ) -> Result<i32, IdeviceError> {
        let wait_future = async {
            loop {
                if let Some(code) = self
                    .find_registered_proxied_channel_code(
                        identifiers,
                        remote_service,
                        remote_channel,
                    )
                    .await
                {
                    return Ok(code);
                }

                if self.shared.closed.load(Ordering::Relaxed) {
                    return Err(Self::closed_error());
                }

                tokio::select! {
                    _ = self.shared.registry_notify.notified() => {}
                    _ = self.shared.closed_notify.notified() => return Err(Self::closed_error()),
                }
            }
        };

        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, wait_future)
                .await
                .map_err(|_| remote_timeout_error(timeout))?,
            None => wait_future.await,
        }
    }

    /// Waits for the code of a proxied service channel whose local or remote
    /// sub-service matches one of `identifiers`.
    pub(crate) async fn wait_for_proxied_service_channel_code(
        &self,
        identifiers: &[&str],
        remote_service: bool,
        remote_channel: Option<bool>,
        timeout: Option<std::time::Duration>,
    ) -> Result<i32, IdeviceError> {
        self.wait_for_proxied_channel_code(identifiers, remote_service, remote_channel, timeout)
            .await
    }

    async fn find_registered_channel_code(
        &self,
        identifiers: &[&str],
        remote: Option<bool>,
    ) -> Option<i32> {
        let metadata = self.shared.channel_metadata.lock().await;
        metadata.values().find_map(|entry| {
            let matches_identifier = identifiers.contains(&entry.identifier.as_str());
            let matches_remote = remote.is_none_or(|remote_flag| remote_flag == entry.remote);
            (matches_identifier && matches_remote).then_some(entry.code)
        })
    }

    async fn find_registered_proxied_channel_code(
        &self,
        identifiers: &[&str],
        remote_service: bool,
        remote_channel: Option<bool>,
    ) -> Option<i32> {
        let metadata = self.shared.channel_metadata.lock().await;
        metadata.values().find_map(|entry| {
            let matches_remote_channel =
                remote_channel.is_none_or(|remote_flag| remote_flag == entry.remote);
            if !matches_remote_channel {
                return None;
            }

            let (local_service, remote_service_name) =
                Self::parse_dtxproxy_identifier(&entry.identifier, entry.remote)?;
            let candidate = if remote_service {
                remote_service_name
            } else {
                local_service
            };

            identifiers.contains(&candidate).then_some(entry.code)
        })
    }

    fn parse_dtxproxy_identifier(identifier: &str, remote_channel: bool) -> Option<(&str, &str)> {
        let mut parts = identifier.split(':');
        let prefix = parts.next()?;
        let first = parts.next()?;
        let second = parts.next()?;
        if prefix != "dtxproxy" || parts.next().is_some() {
            return None;
        }

        if remote_channel {
            Some((second, first))
        } else {
            Some((first, second))
        }
    }

    async fn send_method(
        &self,
        channel: i32,
        identifier: u32,
        data: Option<impl Into<plist::Value>>,
        args: Option<Vec<AuxValue>>,
        expect_reply: bool,
        correlate_reply: bool,
    ) -> Result<Option<oneshot::Receiver<Message>>, IdeviceError> {
        let mheader = MessageHeader::new(0, 1, identifier, 0, channel, expect_reply);
        let pheader = PayloadHeader::method_invocation();
        let aux = args.map(Aux::from_values);
        let data: Option<plist::Value> = data.map(Into::into);

        let message = Message::new(mheader, pheader, aux, data);
        debug!("[{}] Sending message: {message:#?}", self.label);

        let receiver = if correlate_reply {
            let (sender, receiver) = oneshot::channel();
            self.shared
                .pending_replies
                .lock()
                .await
                .insert(identifier, sender);
            Some(receiver)
        } else {
            None
        };

        let write_result = self.shared.write_all(&message.serialize()).await;
        if write_result.is_err() {
            self.shared.pending_replies.lock().await.remove(&identifier);
        }
        write_result?;

        Ok(receiver)
    }

    async fn wait_for_reply(
        &self,
        identifier: u32,
        receiver: oneshot::Receiver<Message>,
    ) -> Result<Message, IdeviceError> {
        match receiver.await {
            Ok(message) => Ok(message),
            Err(_) => {
                self.shared.pending_replies.lock().await.remove(&identifier);
                if self.shared.closed.load(Ordering::Relaxed) {
                    Err(Self::closed_error())
                } else {
                    Err(IdeviceError::UnexpectedResponse(
                        "unexpected response".into(),
                    ))
                }
            }
        }
    }

    /// Calls a method on the specified channel
    ///
    /// # Arguments
    /// * `channel` - Channel number to call method on
    /// * `data` - Optional method data (plist value)
    /// * `args` - Optional arguments for the method
    /// * `expect_reply` - Whether to expect a response
    ///
    /// # Returns
    /// * `Ok(())` - If method was successfully called
    /// * `Err(IdeviceError)` - If call failed
    ///
    /// # Errors
    /// IO or serialization errors
    pub async fn call_method(
        &mut self,
        channel: i32,
        data: Option<impl Into<plist::Value>>,
        args: Option<Vec<AuxValue>>,
        expect_reply: bool,
    ) -> Result<(), IdeviceError> {
        let identifier = self.shared.current_message.fetch_add(1, Ordering::Relaxed) + 1;
        self.send_method(channel, identifier, data, args, expect_reply, false)
            .await?;
        Ok(())
    }

    /// Calls a method and waits for the reply correlated by message identifier.
    pub(crate) async fn call_method_with_reply(
        &mut self,
        channel: i32,
        data: Option<impl Into<plist::Value>>,
        args: Option<Vec<AuxValue>>,
    ) -> Result<Message, IdeviceError> {
        let identifier = self.shared.current_message.fetch_add(1, Ordering::Relaxed) + 1;
        let receiver = self
            .send_method(channel, identifier, data, args, true, true)
            .await?
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))?;
        self.wait_for_reply(identifier, receiver).await
    }

    /// Reads the next message from the specified channel
    ///
    /// Checks cached messages first, then reads from transport if needed.
    ///
    /// # Arguments
    /// * `channel` - Channel number to read from
    ///
    /// # Returns
    /// * `Ok(Message)` - The received message
    /// * `Err(IdeviceError)` - If read failed
    ///
    /// # Errors
    /// * `IdeviceError::UnknownChannel` if channel doesn't exist
    /// * Other IO or deserialization errors
    pub async fn read_message(&mut self, channel: i32) -> Result<Message, IdeviceError> {
        loop {
            let queue = self
                .get_channel_queue(channel)
                .await
                .ok_or_else(|| DvtError::UnknownChannel(channel.unsigned_abs()))?;

            {
                let mut messages = queue.messages.lock().await;
                if let Some(msg) = messages.pop_front() {
                    return Ok(msg);
                }
            }

            if self.shared.closed.load(Ordering::Relaxed) {
                return Err(Self::closed_error());
            }

            tokio::select! {
                _ = queue.notify.notified() => {}
                _ = self.shared.closed_notify.notified() => return Err(Self::closed_error()),
            }
        }
    }

    fn spawn_reader(
        label: Arc<str>,
        shared: Arc<RemoteServerShared<WriteHalf<R>>>,
        mut reader: ReadHalf<R>,
    ) -> JoinHandle<()>
    where
        R: 'static,
    {
        tokio::spawn(async move {
            loop {
                match Message::from_reader(&mut reader).await {
                    Ok(msg) => {
                        debug!("[{}] Read message: {msg:#?}", label);
                        if Self::dispatch_pending_reply(&shared, msg.clone()).await {
                            continue;
                        }
                        if Self::handle_control_message(&shared, &msg).await {
                            continue;
                        }
                        if Self::dispatch_to_handler(&shared, msg.clone()).await {
                            continue;
                        }
                        Self::enqueue_message(&shared, msg).await;
                    }
                    Err(e) => {
                        warn!("[{}] RemoteServer reader exiting: {} ({:?})", label, e, e);
                        Self::fail_pending_replies(&shared).await;
                        shared.closed.store(true, Ordering::Relaxed);
                        shared.closed_notify.notify_waiters();
                        break;
                    }
                }
            }
        })
    }

    async fn handle_control_message(
        shared: &Arc<RemoteServerShared<WriteHalf<R>>>,
        msg: &Message,
    ) -> bool {
        if msg.message_header.channel != 0 {
            return false;
        }

        match msg.data.as_ref() {
            Some(plist::Value::String(selector))
                if selector == "_notifyOfPublishedCapabilities:" =>
            {
                let aux = match msg.aux.as_ref() {
                    Some(aux) => aux.values.as_slice(),
                    None => {
                        warn!("Capabilities notification without aux payload");
                        return true;
                    }
                };

                let Some(first) = aux.first() else {
                    warn!("Capabilities notification missing payload");
                    return true;
                };

                match Self::decode_capabilities(first) {
                    Ok(capabilities) => {
                        debug!("Received remote capabilities: {:?}", capabilities);
                        *shared.supported_identifiers.lock().await =
                            CapabilityHandshakeState::Received(capabilities);
                        shared.handshake_notify.notify_waiters();
                        // Preserve pre-XCTest behavior: older DVT callers expect the
                        // initial capabilities hello to remain observable via
                        // `read_message(0)` on the root channel.
                        Self::enqueue_message(shared, msg.clone()).await;
                    }
                    Err(e) => warn!("Failed to decode remote capabilities: {}", e),
                }
                return true;
            }
            Some(plist::Value::String(selector)) if selector == "_channelCanceled:" => {
                let aux = match msg.aux.as_ref() {
                    Some(aux) => aux.values.as_slice(),
                    None => {
                        warn!("Incoming channel cancellation without aux payload");
                        return true;
                    }
                };

                let Some(first) = aux.first() else {
                    warn!("Incoming channel cancellation missing channel code");
                    return true;
                };

                match Self::decode_channel_code(first) {
                    Ok(channel_code) => {
                        debug!("Remote cancelled channel {}", channel_code);
                        Self::remove_channel(shared, channel_code).await;
                    }
                    Err(e) => warn!("Failed to decode incoming channel cancellation: {}", e),
                }
                return true;
            }
            Some(plist::Value::String(selector))
                if selector == "_requestChannelWithCode:identifier:" => {}
            _ => return false,
        }

        let aux = match msg.aux.as_ref() {
            Some(aux) => aux.values.as_slice(),
            None => {
                warn!("Incoming channel request without aux payload");
                return false;
            }
        };

        if aux.len() < 2 {
            warn!("Incoming channel request missing aux values");
            return false;
        }

        let code = match aux[0] {
            AuxValue::U32(code) => -(code as i32),
            _ => {
                warn!("Incoming channel request aux[0] is not U32");
                return false;
            }
        };

        let identifier = match Self::decode_identifier(&aux[1]) {
            Ok(identifier) => identifier,
            Err(e) => {
                warn!("Failed to decode incoming channel identifier: {}", e);
                return false;
            }
        };

        debug!(
            "Remote requested channel {} with identifier '{}'",
            code, identifier
        );

        shared.channel_metadata.lock().await.insert(
            code,
            ChannelMetadata {
                code,
                identifier: identifier.clone(),
                remote: true,
            },
        );
        shared.registry_notify.notify_waiters();
        Self::ensure_channel_registered_shared(shared, code).await;

        if let Err(error) =
            Self::run_incoming_channel_initializers(shared, code, identifier.clone()).await
        {
            warn!(
                "Failed to initialize incoming channel {} ('{}'): {}",
                code, identifier, error
            );
        }

        if let Err(e) = shared
            .send_raw_reply(
                0,
                msg.message_header.identifier(),
                msg.message_header.conversation_index(),
                &[],
            )
            .await
        {
            warn!("Failed to acknowledge incoming channel request: {}", e);
            shared.closed.store(true, Ordering::Relaxed);
            shared.closed_notify.notify_waiters();
        }

        true
    }

    async fn run_incoming_channel_initializers(
        shared: &Arc<RemoteServerShared<WriteHalf<R>>>,
        channel: i32,
        identifier: String,
    ) -> Result<(), IdeviceError> {
        let initializer = {
            let registrations = shared.incoming_channel_registrations.lock().await;
            registrations
                .iter()
                .find(|registration| {
                    registration
                        .identifiers
                        .iter()
                        .any(|candidate| candidate == &identifier)
                })
                .map(|registration| registration.initializer.clone())
        };

        let Some(initializer) = initializer else {
            return Ok(());
        };

        initializer(shared.label.clone(), shared.clone(), channel, identifier).await
    }

    async fn enqueue_message(shared: &Arc<RemoteServerShared<WriteHalf<R>>>, msg: Message) {
        if msg.message_header.conversation_index() == 0 {
            debug!(
                "Queueing unhandled incoming message on channel {} expects_reply={} data={:?}",
                msg.message_header.channel,
                msg.message_header.expects_reply(),
                msg.data
            );
        }
        if let Some(queue) =
            Self::get_channel_queue_shared(shared, msg.message_header.channel).await
        {
            let notify = &queue.notify;
            {
                let mut messages = queue.messages.lock().await;
                messages.push_back(msg);
            }
            notify.notify_waiters();
        } else {
            warn!(
                "Received message for unknown channel: {}",
                msg.message_header.channel
            );
        }
    }

    async fn dispatch_to_handler(
        shared: &Arc<RemoteServerShared<WriteHalf<R>>>,
        msg: Message,
    ) -> bool {
        if msg.message_header.conversation_index() != 0 {
            return false;
        }

        let handler = {
            let handlers = shared.handlers.lock().await;
            handlers.get(&msg.message_header.channel).cloned()
        };

        let Some(handler) = handler else {
            return false;
        };

        let expects_reply = msg.message_header.expects_reply();
        let msg_id = msg.message_header.identifier();
        let conversation_index = msg.message_header.conversation_index();
        let channel = msg.message_header.channel;

        match handler(msg).await {
            Ok(IncomingHandlerOutcome::Unhandled) => false,
            Ok(IncomingHandlerOutcome::HandledNoReply) => {
                if expects_reply
                    && let Err(e) = shared
                        .send_raw_reply(channel, msg_id, conversation_index, &[])
                        .await
                {
                    warn!("Failed to auto-ack handled incoming message: {}", e);
                }
                true
            }
            Ok(IncomingHandlerOutcome::Reply(reply_bytes)) => {
                if let Err(e) = shared
                    .send_raw_reply(channel, msg_id, conversation_index, &reply_bytes)
                    .await
                {
                    warn!("Failed to reply from incoming handler: {}", e);
                }
                true
            }
            Err(e) => {
                warn!("Incoming message handler failed: {}", e);
                false
            }
        }
    }

    async fn dispatch_pending_reply(
        shared: &Arc<RemoteServerShared<WriteHalf<R>>>,
        msg: Message,
    ) -> bool {
        if msg.message_header.conversation_index() == 0 {
            return false;
        }

        let pending = shared
            .pending_replies
            .lock()
            .await
            .remove(&msg.message_header.identifier());

        let Some(sender) = pending else {
            return false;
        };

        if sender.send(msg).is_err() {
            warn!("Reply waiter dropped before correlated reply was delivered");
        }

        true
    }

    async fn ensure_channel_registered(&self, code: i32) {
        Self::ensure_channel_registered_shared(&self.shared, code).await;
    }

    async fn ensure_channel_registered_shared(
        shared: &Arc<RemoteServerShared<WriteHalf<R>>>,
        code: i32,
    ) {
        let mut channels = shared.channels.lock().await;
        channels
            .entry(code)
            .or_insert_with(|| Arc::new(ChannelQueue::default()));
    }

    async fn get_channel_queue(&self, code: i32) -> Option<Arc<ChannelQueue>> {
        Self::get_channel_queue_shared(&self.shared, code).await
    }

    async fn get_channel_queue_shared(
        shared: &Arc<RemoteServerShared<WriteHalf<R>>>,
        code: i32,
    ) -> Option<Arc<ChannelQueue>> {
        let channels = shared.channels.lock().await;
        channels.get(&code).cloned()
    }

    fn decode_identifier(aux: &AuxValue) -> Result<String, IdeviceError> {
        match aux {
            AuxValue::String(s) => Ok(s.clone()),
            AuxValue::Array(bytes) => {
                match ns_keyed_archive::decode::from_bytes(bytes).map_err(DvtError::from)? {
                    plist::Value::String(s) => Ok(s),
                    _ => Err(IdeviceError::UnexpectedResponse(
                        "unexpected response".into(),
                    )),
                }
            }
            _ => Err(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            )),
        }
    }

    fn decode_capabilities(aux: &AuxValue) -> Result<Dictionary, IdeviceError> {
        match aux {
            AuxValue::Array(bytes) => {
                match ns_keyed_archive::decode::from_bytes(bytes).map_err(DvtError::from)? {
                    plist::Value::Dictionary(dict) => Ok(dict),
                    _ => Err(IdeviceError::UnexpectedResponse(
                        "unexpected response".into(),
                    )),
                }
            }
            _ => Err(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            )),
        }
    }

    fn decode_channel_code(aux: &AuxValue) -> Result<i32, IdeviceError> {
        match aux {
            AuxValue::U32(code) => i32::try_from(*code)
                .map_err(|_| IdeviceError::UnexpectedResponse("unexpected response".into())),
            AuxValue::I64(code) => i32::try_from(*code)
                .map_err(|_| IdeviceError::UnexpectedResponse("unexpected response".into())),
            _ => Err(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            )),
        }
    }

    async fn remove_channel(shared: &Arc<RemoteServerShared<WriteHalf<R>>>, channel_code: i32) {
        shared.handlers.lock().await.remove(&channel_code);
        shared.channels.lock().await.remove(&channel_code);
        shared.channel_metadata.lock().await.remove(&channel_code);
        shared.registry_notify.notify_waiters();
    }

    async fn fail_pending_replies(shared: &Arc<RemoteServerShared<WriteHalf<R>>>) {
        shared.pending_replies.lock().await.clear();
    }

    fn closed_error() -> IdeviceError {
        IdeviceError::Socket(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "remote server connection closed",
        ))
    }
}

impl RemoteServerClient<Box<dyn ReadWrite>> {
    /// Creates a new RemoteServerClient with the given transport.
    pub fn new(idevice: impl ReadWrite + 'static) -> Self {
        Self::with_label(idevice, "remote-server")
    }

    /// Creates a new client with a debug label used in tracing output.
    pub fn with_label(idevice: impl ReadWrite + 'static, label: impl Into<String>) -> Self {
        Self::with_label_typed(Box::new(idevice), label)
    }
}

impl<R: ReadWrite> Drop for RemoteServerClient<R> {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

impl<W: tokio::io::AsyncWrite + Unpin> RemoteServerShared<W> {
    async fn write_all(&self, bytes: &[u8]) -> Result<(), IdeviceError> {
        let mut writer = self.writer.lock().await;
        writer.write_all(bytes).await?;
        writer.flush().await?;
        Ok(())
    }

    async fn send_raw_reply(
        &self,
        channel: i32,
        incoming_msg_id: u32,
        incoming_conversation_index: u32,
        data_bytes: &[u8],
    ) -> Result<(), IdeviceError> {
        let buf = Message::build_raw_reply(
            channel,
            incoming_msg_id,
            incoming_conversation_index,
            data_bytes,
        );
        self.write_all(&buf).await
    }
}

impl<R: ReadWrite> Channel<'_, R> {
    /// Converts this borrowed channel handle into an owned/shared one.
    pub(crate) fn detach(&self) -> OwnedChannel<R> {
        OwnedChannel {
            label: self.client.label.clone(),
            shared: self.client.shared.clone(),
            channel: self.channel,
        }
    }

    /// Reads the next message from the remote server on this channel
    ///
    /// # Returns
    /// * `Ok(Message)` - The received message
    /// * `Err(IdeviceError)` - If read failed
    ///
    /// # Errors
    /// * `IdeviceError::UnknownChannel` if channel doesn't exist
    /// * Other IO or deserialization errors
    pub async fn read_message(&mut self) -> Result<Message, IdeviceError> {
        self.client.read_message(self.channel).await
    }

    /// Calls a method on the specified channel
    ///
    /// # Arguments
    /// * `method` - Optional method data (plist value)
    /// * `args` - Optional arguments for the method
    /// * `expect_reply` - Whether to expect a response
    ///
    /// # Returns
    /// * `Ok(())` - If method was successfully called
    /// * `Err(IdeviceError)` - If call failed
    ///
    /// # Errors
    /// IO or serialization errors
    pub async fn call_method(
        &mut self,
        method: Option<impl Into<plist::Value>>,
        args: Option<Vec<AuxValue>>,
        expect_reply: bool,
    ) -> Result<(), IdeviceError> {
        self.client
            .call_method(self.channel, method, args, expect_reply)
            .await
    }

    /// Calls a method on this channel and waits for the correlated reply.
    pub(crate) async fn call_method_with_reply(
        &mut self,
        method: Option<impl Into<plist::Value>>,
        args: Option<Vec<AuxValue>>,
    ) -> Result<Message, IdeviceError> {
        self.client
            .call_method_with_reply(self.channel, method, args)
            .await
    }
}

impl<R: ReadWrite + 'static> OwnedChannel<R> {
    /// Reads the next queued message from this channel.
    pub async fn read_message(&mut self) -> Result<Message, IdeviceError> {
        loop {
            let queue =
                RemoteServerClient::<R>::get_channel_queue_shared(&self.shared, self.channel)
                    .await
                    .ok_or_else(|| DvtError::UnknownChannel(self.channel.unsigned_abs()))?;

            {
                let mut messages = queue.messages.lock().await;
                if let Some(msg) = messages.pop_front() {
                    return Ok(msg);
                }
            }

            if self.shared.closed.load(Ordering::Relaxed) {
                return Err(RemoteServerClient::<R>::closed_error());
            }

            tokio::select! {
                _ = queue.notify.notified() => {}
                _ = self.shared.closed_notify.notified() => {
                    return Err(RemoteServerClient::<R>::closed_error())
                }
            }
        }
    }

    /// Reads the next queued message with a timeout.
    pub(crate) async fn read_message_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Message, IdeviceError> {
        tokio::time::timeout(timeout, self.read_message())
            .await
            .map_err(|_| remote_timeout_error(timeout))?
    }

    /// Calls a method on this channel.
    pub async fn call_method(
        &mut self,
        method: Option<impl Into<plist::Value>>,
        args: Option<Vec<AuxValue>>,
        expect_reply: bool,
    ) -> Result<(), IdeviceError> {
        let identifier = self.shared.current_message.fetch_add(1, Ordering::Relaxed) + 1;
        let mheader = MessageHeader::new(0, 1, identifier, 0, self.channel, expect_reply);
        let pheader = PayloadHeader::method_invocation();
        let aux = args.map(Aux::from_values);
        let data: Option<plist::Value> = method.map(Into::into);
        let message = Message::new(mheader, pheader, aux, data);
        debug!("[{}] Sending message: {message:#?}", self.label);

        self.shared.write_all(&message.serialize()).await?;

        Ok(())
    }

    /// Calls a method on this channel and waits for the correlated reply.
    pub(crate) async fn call_method_with_reply(
        &mut self,
        method: Option<impl Into<plist::Value>>,
        args: Option<Vec<AuxValue>>,
    ) -> Result<Message, IdeviceError> {
        let identifier = self.shared.current_message.fetch_add(1, Ordering::Relaxed) + 1;
        let mheader = MessageHeader::new(0, 1, identifier, 0, self.channel, true);
        let pheader = PayloadHeader::method_invocation();
        let aux = args.map(Aux::from_values);
        let data: Option<plist::Value> = method.map(Into::into);
        let message = Message::new(mheader, pheader, aux, data);
        debug!("[{}] Sending message: {message:#?}", self.label);

        let (sender, receiver) = oneshot::channel::<Message>();
        self.shared
            .pending_replies
            .lock()
            .await
            .insert(identifier, sender);

        let write_result = self.shared.write_all(&message.serialize()).await;
        if write_result.is_err() {
            self.shared.pending_replies.lock().await.remove(&identifier);
        }
        write_result?;

        match receiver.await {
            Ok(message) => Ok(message),
            Err(_) => {
                self.shared.pending_replies.lock().await.remove(&identifier);
                if self.shared.closed.load(Ordering::Relaxed) {
                    Err(RemoteServerClient::<R>::closed_error())
                } else {
                    Err(IdeviceError::UnexpectedResponse(
                        "unexpected response".into(),
                    ))
                }
            }
        }
    }

    /// Registers an incoming handler for this channel.
    pub(crate) async fn set_incoming_handler<F, Fut>(&mut self, handler: F)
    where
        F: Fn(Message) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<IncomingHandlerOutcome, IdeviceError>> + Send + 'static,
    {
        let handler: IncomingMessageHandler = Arc::new(move |msg| Box::pin(handler(msg)));
        self.shared
            .handlers
            .lock()
            .await
            .insert(self.channel, handler);
    }

    /// Removes the incoming handler for this channel.
    pub(crate) async fn clear_incoming_handler(&mut self) {
        self.shared.handlers.lock().await.remove(&self.channel);
    }

    /// Sends a raw reply for an incoming message on this channel.
    pub(crate) async fn send_raw_reply_for(
        &mut self,
        incoming_msg_id: u32,
        incoming_conversation_index: u32,
        data_bytes: &[u8],
    ) -> Result<(), IdeviceError> {
        self.shared
            .send_raw_reply(
                self.channel,
                incoming_msg_id,
                incoming_conversation_index,
                data_bytes,
            )
            .await
    }
}
