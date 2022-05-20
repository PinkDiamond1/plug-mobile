//! The main Agent module. Contains the [Agent] type and all associated structures.
pub(crate) mod agent_config;
pub mod agent_error;
pub(crate) mod builder;
pub mod http_transport;
pub(crate) mod nonce;
pub(crate) mod replica_api;
pub(crate) mod response;
pub(crate) mod response_authentication;

pub mod signed;
pub mod status;
pub use agent_config::AgentConfig;
pub use agent_error::AgentError;
pub use builder::AgentBuilder;
pub use nonce::{NonceFactory, NonceGenerator};
pub use response::{Replied, RequestStatusResponse};

use serde::Deserialize;
use serde_json::json;

#[cfg(test)]
mod agent_test;

use crate::{
    agent::replica_api::{
        CallRequestContent, Certificate, Delegation, Envelope, QueryContent, ReadStateContent,
        ReadStateResponse,
    },
    export::Principal,
    hash_tree::Label,
    identity::Identity,
    to_request_id, RequestId,
};
use garcon::Waiter;
use serde::Serialize;
use status::Status;

use crate::{
    agent::response_authentication::{
        extract_der, initialize_bls, lookup_canister_info, lookup_canister_metadata,
        lookup_request_status, lookup_value,
    },
    bls::bls12381::bls,
};
use std::{
    convert::TryFrom,
    ffi::{CStr, CString},
    future::Future,
    os::raw::c_char,
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Context, Poll},
    time::Duration,
};

const IC_REQUEST_DOMAIN_SEPARATOR: &[u8; 11] = b"\x0Aic-request";
const IC_STATE_ROOT_DOMAIN_SEPARATOR: &[u8; 14] = b"\x0Dic-state-root";

const IC_ROOT_KEY: &[u8; 133] = b"\x30\x81\x82\x30\x1d\x06\x0d\x2b\x06\x01\x04\x01\x82\xdc\x7c\x05\x03\x01\x02\x01\x06\x0c\x2b\x06\x01\x04\x01\x82\xdc\x7c\x05\x03\x02\x01\x03\x61\x00\x81\x4c\x0e\x6e\xc7\x1f\xab\x58\x3b\x08\xbd\x81\x37\x3c\x25\x5c\x3c\x37\x1b\x2e\x84\x86\x3c\x98\xa4\xf1\xe0\x8b\x74\x23\x5d\x14\xfb\x5d\x9c\x0c\xd5\x46\xd9\x68\x5f\x91\x3a\x0c\x0b\x2c\xc5\x34\x15\x83\xbf\x4b\x43\x92\xe4\x67\xdb\x96\xd6\x5b\x9b\xb4\xcb\x71\x71\x12\xf8\x47\x2e\x0d\x5a\x4d\x14\x50\x5f\xfd\x74\x84\xb0\x12\x91\x09\x1c\x5f\x87\xb9\x88\x83\x46\x3f\x98\x09\x1a\x0b\xaa\xae";

/// A facade that connects to a Replica and does requests. These requests can be of any type
/// (does not have to be HTTP). This trait is to inverse the control from the Agent over its
/// connection code, and to resolve any direct dependencies to tokio or HTTP code from this
/// crate.
///
/// An implementation of this trait for HTTP transport is implemented using Reqwest, with the
/// feature flag `reqwest`. This might be deprecated in the future.
///
/// Any error returned by these methods will bubble up to the code that called the [Agent].
pub trait ReplicaV2Transport: Send + Sync {
    /// Sends an asynchronous request to a Replica. The Request ID is non-mutable and
    /// depends on the content of the envelope.
    ///
    /// This normally corresponds to the `/api/v2/canister/<effective_canister_id>/call` endpoint.
    fn call<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
        request_id: RequestId,
    ) -> Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + 'a>>;

    /// Sends a synchronous request to a Replica. This call includes the body of the request message
    /// itself (envelope).
    ///
    /// This normally corresponds to the `/api/v2/canister/<effective_canister_id>/read_state` endpoint.
    fn read_state<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>>;

    /// Sends a synchronous request to a Replica. This call includes the body of the request message
    /// itself (envelope).
    ///
    /// This normally corresponds to the `/api/v2/canister/<effective_canister_id>/query` endpoint.
    fn query<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>>;

    /// Sends a status request to the Replica, returning whatever the replica returns.
    /// In the current spec v2, this is a CBOR encoded status message, but we are not
    /// making this API attach semantics to the response.
    fn status<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>>;
}

impl<I: ReplicaV2Transport + ?Sized> ReplicaV2Transport for Box<I> {
    fn call<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
        request_id: RequestId,
    ) -> Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + 'a>> {
        (**self).call(effective_canister_id, envelope, request_id)
    }
    fn read_state<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>> {
        (**self).read_state(effective_canister_id, envelope)
    }
    fn query<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>> {
        (**self).query(effective_canister_id, envelope)
    }
    fn status<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>> {
        (**self).status()
    }
}
impl<I: ReplicaV2Transport + ?Sized> ReplicaV2Transport for Arc<I> {
    fn call<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
        request_id: RequestId,
    ) -> Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + 'a>> {
        (**self).call(effective_canister_id, envelope, request_id)
    }
    fn read_state<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>> {
        (**self).read_state(effective_canister_id, envelope)
    }
    fn query<'a>(
        &'a self,
        effective_canister_id: Principal,
        envelope: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>> {
        (**self).query(effective_canister_id, envelope)
    }
    fn status<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AgentError>> + Send + 'a>> {
        (**self).status()
    }
}

/// Classification of the result of a request_status_raw (poll) call.
pub enum PollResult {
    /// The request has been submitted, but we do not know yet if it
    /// has been accepted or not.
    Submitted,

    /// The request has been received and may be processing.
    Accepted,

    /// The request completed and returned some data.
    Completed(Vec<u8>),
}

/// A low level Agent to make calls to a Replica endpoint.
///
/// ```ignore
/// # // This test is ignored because it requires an ic to be running. We run these
/// # // in the ic-ref workflow.
/// use ic_agent::{Agent, ic_types::Principal};
/// use candid::{Encode, Decode, CandidType, Nat};
/// use serde::Deserialize;
///
/// #[derive(CandidType)]
/// struct Argument {
///   amount: Option<Nat>,
/// }
///
/// #[derive(CandidType, Deserialize)]
/// struct CreateCanisterResult {
///   canister_id: Principal,
/// }
///
/// # fn create_identity() -> impl ic_agent::Identity {
/// #     let rng = ring::rand::SystemRandom::new();
/// #     let key_pair = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng)
/// #         .expect("Could not generate a key pair.");
/// #
/// #     ic_agent::identity::BasicIdentity::from_key_pair(
/// #         ring::signature::Ed25519KeyPair::from_pkcs8(key_pair.as_ref())
/// #           .expect("Could not read the key pair."),
/// #     )
/// # }
/// #
/// # const URL: &'static str = concat!("http://localhost:", env!("IC_REF_PORT"));
/// #
/// async fn create_a_canister() -> Result<Principal, Box<dyn std::error::Error>> {
///   let agent = Agent::builder()
///     .with_url(URL)
///     .with_identity(create_identity())
///     .build()?;
///
///   // Only do the following call when not contacting the IC main net (e.g. a local emulator).
///   // This is important as the main net public key is static and a rogue network could return
///   // a different key.
///   // If you know the root key ahead of time, you can use `agent.set_root_key(root_key)?;`.
///   agent.fetch_root_key().await?;
///   let management_canister_id = Principal::from_text("aaaaa-aa")?;
///
///   let waiter = garcon::Delay::builder()
///     .throttle(std::time::Duration::from_millis(500))
///     .timeout(std::time::Duration::from_secs(60 * 5))
///     .build();
///
///   // Create a call to the management canister to create a new canister ID,
///   // and wait for a result.
///   let response = agent.update(&management_canister_id, "provisional_create_canister_with_cycles")
///     .with_arg(&Encode!(&Argument { amount: None })?)
///     .call_and_wait(waiter)
///     .await?;
///
///   let result = Decode!(response.as_slice(), CreateCanisterResult)?;
///   let canister_id: Principal = Principal::from_text(&result.canister_id.to_text())?;
///   Ok(canister_id)
/// }
///
/// # let mut runtime = tokio::runtime::Runtime::new().unwrap();
/// # runtime.block_on(async {
/// let canister_id = create_a_canister().await.unwrap();
/// eprintln!("{}", canister_id);
/// # });
/// ```
///
/// This agent does not understand Candid, and only acts on byte buffers.
#[derive(Clone)]
pub struct Agent {
    nonce_factory: Arc<dyn NonceGenerator>,
    identity: Arc<dyn Identity>,
    ingress_expiry_duration: Duration,
    root_key: Arc<RwLock<Option<Vec<u8>>>>,
    transport: Arc<dyn ReplicaV2Transport>,
}

impl Agent {
    /// Create an instance of an [`AgentBuilder`] for building an [`Agent`]. This is simpler than
    /// using the [`AgentConfig`] and [`Agent::new()`].
    pub fn builder() -> builder::AgentBuilder {
        Default::default()
    }

    /// Create an instance of an [`Agent`].
    pub fn new(config: agent_config::AgentConfig) -> Result<Agent, AgentError> {
        initialize_bls()?;

        Ok(Agent {
            nonce_factory: config.nonce_factory,
            identity: config.identity,
            ingress_expiry_duration: config
                .ingress_expiry_duration
                .unwrap_or_else(|| Duration::from_secs(300)),
            root_key: Arc::new(RwLock::new(Some(IC_ROOT_KEY.to_vec()))),
            transport: config
                .transport
                .ok_or_else(AgentError::MissingReplicaTransport)?,
        })
    }

    /// Set the transport of the [`Agent`].
    pub fn set_transport<F: 'static + ReplicaV2Transport>(&mut self, transport: F) {
        self.transport = Arc::new(transport);
    }

    /// By default, the agent is configured to talk to the main Internet Computer, and verifies
    /// responses using a hard-coded public key.
    ///
    /// This function will instruct the agent to ask the endpoint for its public key, and use
    /// that instead. This is required when talking to a local test instance, for example.
    ///
    /// *Only use this when you are  _not_ talking to the main Internet Computer, otherwise
    /// you are prone to man-in-the-middle attacks! Do not call this function by default.*
    pub async fn fetch_root_key(&self) -> Result<(), AgentError> {
        let status = self.status().await?;
        let root_key = status
            .root_key
            .clone()
            .ok_or(AgentError::NoRootKeyInStatus(status))?;
        self.set_root_key(root_key)
    }

    /// By default, the agent is configured to talk to the main Internet Computer, and verifies
    /// responses using a hard-coded public key.
    ///
    /// Using this function you can set the root key to a known one if you know if beforehand.
    pub fn set_root_key(&self, root_key: Vec<u8>) -> Result<(), AgentError> {
        if let Ok(mut write_guard) = self.root_key.write() {
            *write_guard = Some(root_key);
        }
        Ok(())
    }

    fn read_root_key(&self) -> Result<Vec<u8>, AgentError> {
        if let Ok(read_lock) = self.root_key.read() {
            if let Some(root_key) = read_lock.clone() {
                Ok(root_key)
            } else {
                Err(AgentError::CouldNotReadRootKey())
            }
        } else {
            Err(AgentError::CouldNotReadRootKey())
        }
    }

    fn get_expiry_date(&self) -> u64 {
        // TODO(hansl): evaluate if we need this on the agent side (my hunch is we don't).
        let permitted_drift = Duration::from_secs(60);
        (self
            .ingress_expiry_duration
            .as_nanos()
            .saturating_add(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("Time wrapped around.")
                    .as_nanos(),
            )
            .saturating_sub(permitted_drift.as_nanos())) as u64
    }

    /// Return the principal of the identity.
    pub fn get_principal(&self) -> Result<Principal, String> {
        self.identity.sender()
    }

    async fn query_endpoint<A>(
        &self,
        effective_canister_id: Principal,
        serialized_bytes: Vec<u8>,
    ) -> Result<A, AgentError>
    where
        A: serde::de::DeserializeOwned,
    {
        let bytes = self
            .transport
            .query(effective_canister_id, serialized_bytes)
            .await?;
        serde_cbor::from_slice(&bytes).map_err(AgentError::InvalidCborData)
    }

    async fn read_state_endpoint<A>(
        &self,
        effective_canister_id: Principal,
        serialized_bytes: Vec<u8>,
    ) -> Result<A, AgentError>
    where
        A: serde::de::DeserializeOwned,
    {
        let bytes = self
            .transport
            .read_state(effective_canister_id, serialized_bytes)
            .await?;
        serde_cbor::from_slice(&bytes).map_err(AgentError::InvalidCborData)
    }

    async fn call_endpoint(
        &self,
        effective_canister_id: Principal,
        request_id: RequestId,
        serialized_bytes: Vec<u8>,
    ) -> Result<RequestId, AgentError> {
        self.transport
            .call(effective_canister_id, serialized_bytes, request_id)
            .await?;
        Ok(request_id)
    }

    /// The simplest way to do a query call; sends a byte array and will return a byte vector.
    /// The encoding is left as an exercise to the user.
    async fn query_raw(
        &self,
        canister_id: &Principal,
        effective_canister_id: Principal,
        method_name: &str,
        arg: &[u8],
        ingress_expiry_datetime: Option<u64>,
    ) -> Result<Vec<u8>, AgentError> {
        let request = self.query_content(canister_id, method_name, arg, ingress_expiry_datetime)?;
        let serialized_bytes = sign_request(&request, self.identity.clone())?;
        self.query_endpoint::<replica_api::QueryResponse>(effective_canister_id, serialized_bytes)
            .await
            .and_then(|response| match response {
                replica_api::QueryResponse::Replied { reply } => Ok(reply.arg),
                replica_api::QueryResponse::Rejected {
                    reject_code,
                    reject_message,
                } => Err(AgentError::ReplicaError {
                    reject_code,
                    reject_message,
                }),
            })
    }

    /// Send the signed query to the network. Will return a byte vector.
    /// The bytes will be checked if it is a valid query.
    /// If you want to inspect the fields of the query call, use [`signed_query_inspect`] before calling this method.
    pub async fn query_signed(
        &self,
        effective_canister_id: Principal,
        signed_query: Vec<u8>,
    ) -> Result<Vec<u8>, AgentError> {
        let _envelope: Envelope<QueryContent> =
            serde_cbor::from_slice(&signed_query).map_err(AgentError::InvalidCborData)?;
        self.query_endpoint::<replica_api::QueryResponse>(effective_canister_id, signed_query)
            .await
            .and_then(|response| match response {
                replica_api::QueryResponse::Replied { reply } => Ok(reply.arg),
                replica_api::QueryResponse::Rejected {
                    reject_code,
                    reject_message,
                } => Err(AgentError::ReplicaError {
                    reject_code,
                    reject_message,
                }),
            })
    }

    fn query_content(
        &self,
        canister_id: &Principal,
        method_name: &str,
        arg: &[u8],
        ingress_expiry_datetime: Option<u64>,
    ) -> Result<QueryContent, AgentError> {
        Ok(QueryContent::QueryRequest {
            sender: self.identity.sender().map_err(AgentError::SigningError)?,
            canister_id: *canister_id,
            method_name: method_name.to_string(),
            arg: arg.to_vec(),
            ingress_expiry: ingress_expiry_datetime.unwrap_or_else(|| self.get_expiry_date()),
        })
    }

    /// The simplest way to do an update call; sends a byte array and will return a RequestId.
    /// The RequestId should then be used for request_status (most likely in a loop).
    async fn update_raw(
        &self,
        canister_id: &Principal,
        effective_canister_id: Principal,
        method_name: &str,
        arg: &[u8],
        ingress_expiry_datetime: Option<u64>,
    ) -> Result<RequestId, AgentError> {
        let request =
            self.update_content(canister_id, method_name, arg, ingress_expiry_datetime)?;
        let request_id = to_request_id(&request)?;
        let serialized_bytes = sign_request(&request, self.identity.clone())?;

        self.call_endpoint(effective_canister_id, request_id, serialized_bytes)
            .await
    }

    /// Send the signed update to the network. Will return a [`RequestId`].
    /// The bytes will be checked to verify that it is a valid update.
    /// If you want to inspect the fields of the update, use [`signed_update_inspect`] before calling this method.
    pub async fn update_signed(
        &self,
        effective_canister_id: Principal,
        signed_update: Vec<u8>,
    ) -> Result<RequestId, AgentError> {
        let envelope: Envelope<CallRequestContent> =
            serde_cbor::from_slice(&signed_update).map_err(AgentError::InvalidCborData)?;
        let request_id = to_request_id(&envelope.content)?;
        self.call_endpoint(effective_canister_id, request_id, signed_update)
            .await
    }

    fn update_content(
        &self,
        canister_id: &Principal,
        method_name: &str,
        arg: &[u8],
        ingress_expiry_datetime: Option<u64>,
    ) -> Result<CallRequestContent, AgentError> {
        Ok(CallRequestContent::CallRequest {
            canister_id: *canister_id,
            method_name: method_name.into(),
            arg: arg.to_vec(),
            nonce: self.nonce_factory.generate().map(|b| b.as_slice().into()),
            sender: self.identity.sender().map_err(AgentError::SigningError)?,
            ingress_expiry: ingress_expiry_datetime.unwrap_or_else(|| self.get_expiry_date()),
        })
    }

    // Call request_status on the RequestId once and classify the result
    pub async fn poll(
        &self,
        request_id: &RequestId,
        effective_canister_id: &Principal,
    ) -> Result<PollResult, AgentError> {
        match self
            .request_status_raw(request_id, *effective_canister_id)
            .await?
        {
            RequestStatusResponse::Unknown => Ok(PollResult::Submitted),

            RequestStatusResponse::Received | RequestStatusResponse::Processing => {
                Ok(PollResult::Accepted)
            }

            RequestStatusResponse::Replied {
                reply: Replied::CallReplied(arg),
            } => Ok(PollResult::Completed(arg)),

            RequestStatusResponse::Rejected {
                reject_code,
                reject_message,
            } => Err(AgentError::ReplicaError {
                reject_code,
                reject_message,
            }),
            RequestStatusResponse::Done => Err(AgentError::RequestStatusDoneNoReply(String::from(
                *request_id,
            ))),
        }
    }

    // Call request_status on the RequestId in a loop and return the response as a byte vector.
    pub async fn wait<W: Waiter>(
        &self,
        request_id: RequestId,
        effective_canister_id: &Principal,
        mut waiter: W,
    ) -> Result<Vec<u8>, AgentError> {
        waiter.start();
        let mut request_accepted = false;
        loop {
            match self.poll(&request_id, effective_canister_id).await? {
                PollResult::Submitted => {}
                PollResult::Accepted => {
                    if !request_accepted {
                        // The system will return RequestStatusResponse::Unknown
                        // (PollResult::Submitted) until the request is accepted
                        // and we generally cannot know how long that will take.
                        // State transitions between Received and Processing may be
                        // instantaneous. Therefore, once we know the request is accepted,
                        // we should restart the waiter so the request does not time out.

                        waiter
                            .restart()
                            .map_err(|_| AgentError::WaiterRestartError())?;
                        request_accepted = true;
                    }
                }
                PollResult::Completed(result) => return Ok(result),
            };

            waiter
                .async_wait()
                .await
                .map_err(|_| AgentError::TimeoutWaitingForResponse())?;
        }
    }

    pub async fn read_state_raw(
        &self,
        paths: Vec<Vec<Label>>,
        effective_canister_id: Principal,
    ) -> Result<Certificate<'_>, AgentError> {
        let request = self.read_state_content(paths)?;
        let serialized_bytes = sign_request(&request, self.identity.clone())?;

        let read_state_response: ReadStateResponse = self
            .read_state_endpoint(effective_canister_id, serialized_bytes)
            .await?;

        let cert: Certificate = serde_cbor::from_slice(&read_state_response.certificate)
            .map_err(AgentError::InvalidCborData)?;
        self.verify(&cert)?;
        Ok(cert)
    }

    fn read_state_content(&self, paths: Vec<Vec<Label>>) -> Result<ReadStateContent, AgentError> {
        Ok(ReadStateContent::ReadStateRequest {
            sender: self.identity.sender().map_err(AgentError::SigningError)?,
            paths,
            ingress_expiry: self.get_expiry_date(),
        })
    }

    /// Verify a certificate, checking delegation if present.
    pub fn verify(&self, cert: &Certificate) -> Result<(), AgentError> {
        let sig = &cert.signature;

        let root_hash = cert.tree.digest();
        let mut msg = vec![];
        msg.extend_from_slice(IC_STATE_ROOT_DOMAIN_SEPARATOR);
        msg.extend_from_slice(&root_hash);

        let der_key = self.check_delegation(&cert.delegation)?;
        let key = extract_der(der_key)?;

        let result = bls::core_verify(sig, &*msg, &*key);
        if result != bls::BLS_OK {
            Err(AgentError::CertificateVerificationFailed())
        } else {
            Ok(())
        }
    }

    fn check_delegation(&self, delegation: &Option<Delegation>) -> Result<Vec<u8>, AgentError> {
        match delegation {
            None => self.read_root_key(),
            Some(delegation) => {
                let cert: Certificate = serde_cbor::from_slice(&delegation.certificate)
                    .map_err(AgentError::InvalidCborData)?;
                self.verify(&cert)?;
                let public_key_path = [
                    "subnet".into(),
                    delegation.subnet_id.clone().into(),
                    "public_key".into(),
                ];
                lookup_value(&cert, public_key_path).map(|pk| pk.to_vec())
            }
        }
    }

    pub async fn read_state_canister_info(
        &self,
        canister_id: Principal,
        path: &str,
    ) -> Result<Vec<u8>, AgentError> {
        let paths: Vec<Vec<Label>> = vec![vec!["canister".into(), canister_id.into(), path.into()]];

        let cert = self.read_state_raw(paths, canister_id).await?;

        lookup_canister_info(cert, canister_id, path)
    }

    pub async fn read_state_canister_metadata(
        &self,
        canister_id: Principal,
        path: &str,
    ) -> Result<Vec<u8>, AgentError> {
        let paths: Vec<Vec<Label>> = vec![vec![
            "canister".into(),
            canister_id.into(),
            "metadata".into(),
            path.into(),
        ]];

        let cert = self.read_state_raw(paths, canister_id).await?;

        lookup_canister_metadata(cert, canister_id, path)
    }

    pub async fn request_status_raw(
        &self,
        request_id: &RequestId,
        effective_canister_id: Principal,
    ) -> Result<RequestStatusResponse, AgentError> {
        let paths: Vec<Vec<Label>> =
            vec![vec!["request_status".into(), request_id.to_vec().into()]];

        let cert = self.read_state_raw(paths, effective_canister_id).await?;

        lookup_request_status(cert, request_id)
    }

    /// Send the signed request_status to the network. Will return [`RequestStatusResponse`].
    /// The bytes will be checked to verify that it is a valid request_status.
    /// If you want to inspect the fields of the request_status, use [`signed_request_status_inspect`] before calling this method.
    pub async fn request_status_signed(
        &self,
        request_id: &RequestId,
        effective_canister_id: Principal,
        signed_request_status: Vec<u8>,
    ) -> Result<RequestStatusResponse, AgentError> {
        let _envelope: Envelope<ReadStateContent> =
            serde_cbor::from_slice(&signed_request_status).map_err(AgentError::InvalidCborData)?;
        let read_state_response: ReadStateResponse = self
            .read_state_endpoint(effective_canister_id, signed_request_status)
            .await?;

        let cert: Certificate = serde_cbor::from_slice(&read_state_response.certificate)
            .map_err(AgentError::InvalidCborData)?;
        self.verify(&cert)?;
        lookup_request_status(cert, request_id)
    }

    /// Returns an UpdateBuilder enabling the construction of an update call without
    /// passing all arguments.
    pub fn update<S: Into<String>>(
        &self,
        canister_id: &Principal,
        method_name: S,
    ) -> UpdateBuilder {
        UpdateBuilder::new(self, *canister_id, method_name.into())
    }

    /// Calls and returns the information returned by the status endpoint of a replica.
    pub async fn status(&self) -> Result<Status, AgentError> {
        let bytes = self.transport.status().await?;

        let cbor: serde_cbor::Value =
            serde_cbor::from_slice(&bytes).map_err(AgentError::InvalidCborData)?;

        Status::try_from(&cbor).map_err(|_| AgentError::InvalidReplicaStatus)
    }

    /// Returns a QueryBuilder enabling the construction of a query call without
    /// passing all arguments.
    pub fn query<S: Into<String>>(&self, canister_id: &Principal, method_name: S) -> QueryBuilder {
        QueryBuilder::new(self, *canister_id, method_name.into())
    }

    /// Sign a request_status call. This will return a [`signed::SignedRequestStatus`]
    /// which contains all fields of the request_status and the signed request_status in CBOR encoding
    pub fn sign_request_status(
        &self,
        effective_canister_id: Principal,
        request_id: RequestId,
    ) -> Result<signed::SignedRequestStatus, AgentError> {
        let paths: Vec<Vec<Label>> =
            vec![vec!["request_status".into(), request_id.to_vec().into()]];
        let read_state_content = self.read_state_content(paths)?;
        let signed_request_status = sign_request(&read_state_content, self.identity.clone())?;
        match read_state_content {
            ReadStateContent::ReadStateRequest {
                ingress_expiry,
                sender,
                paths: _path,
            } => Ok(signed::SignedRequestStatus {
                ingress_expiry,
                sender,
                effective_canister_id,
                request_id,
                signed_request_status,
            }),
        }
    }
}

fn construct_message(request_id: &RequestId) -> Vec<u8> {
    let mut buf = vec![];
    buf.extend_from_slice(IC_REQUEST_DOMAIN_SEPARATOR);
    buf.extend_from_slice(request_id.as_slice());
    buf
}

fn sign_request<'a, V>(request: &V, identity: Arc<dyn Identity>) -> Result<Vec<u8>, AgentError>
where
    V: 'a + Serialize,
{
    let request_id = to_request_id(&request)?;
    let msg = construct_message(&request_id);
    let signature = identity.sign(&msg).map_err(AgentError::SigningError)?;

    let envelope = Envelope {
        content: request,
        sender_pubkey: signature.public_key,
        sender_sig: signature.signature,
    };

    let mut serialized_bytes = Vec::new();
    let mut serializer = serde_cbor::Serializer::new(&mut serialized_bytes);
    serializer.self_describe()?;
    envelope.serialize(&mut serializer)?;

    Ok(serialized_bytes)
}

/// Inspect the bytes to be sent as a query
/// Return Ok only when the bytes can be deserialized as a query and all fields match with the arguments
pub fn signed_query_inspect(
    sender: Principal,
    canister_id: Principal,
    method_name: &str,
    arg: &[u8],
    ingress_expiry: u64,
    signed_query: Vec<u8>,
) -> Result<(), AgentError> {
    let envelope: Envelope<QueryContent> =
        serde_cbor::from_slice(&signed_query).map_err(AgentError::InvalidCborData)?;
    match envelope.content {
        QueryContent::QueryRequest {
            ingress_expiry: ingress_expiry_cbor,
            sender: sender_cbor,
            canister_id: canister_id_cbor,
            method_name: method_name_cbor,
            arg: arg_cbor,
        } => {
            if ingress_expiry != ingress_expiry_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "ingress_expiry".to_string(),
                    value_arg: ingress_expiry.to_string(),
                    value_cbor: ingress_expiry_cbor.to_string(),
                });
            }
            if sender != sender_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "sender".to_string(),
                    value_arg: sender.to_string(),
                    value_cbor: sender_cbor.to_string(),
                });
            }
            if canister_id != canister_id_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "canister_id".to_string(),
                    value_arg: canister_id.to_string(),
                    value_cbor: canister_id_cbor.to_string(),
                });
            }
            if method_name != method_name_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "method_name".to_string(),
                    value_arg: method_name.to_string(),
                    value_cbor: method_name_cbor,
                });
            }
            if arg != arg_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "arg".to_string(),
                    value_arg: format!("{:?}", arg),
                    value_cbor: format!("{:?}", arg_cbor),
                });
            }
        }
    }
    Ok(())
}

/// Inspect the bytes to be sent as an update
/// Return Ok only when the bytes can be deserialized as an update and all fields match with the arguments
pub fn signed_update_inspect(
    sender: Principal,
    canister_id: Principal,
    method_name: &str,
    arg: &[u8],
    ingress_expiry: u64,
    signed_update: Vec<u8>,
) -> Result<(), AgentError> {
    let envelope: Envelope<CallRequestContent> =
        serde_cbor::from_slice(&signed_update).map_err(AgentError::InvalidCborData)?;
    match envelope.content {
        CallRequestContent::CallRequest {
            nonce: _nonce,
            ingress_expiry: ingress_expiry_cbor,
            sender: sender_cbor,
            canister_id: canister_id_cbor,
            method_name: method_name_cbor,
            arg: arg_cbor,
        } => {
            if ingress_expiry != ingress_expiry_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "ingress_expiry".to_string(),
                    value_arg: ingress_expiry.to_string(),
                    value_cbor: ingress_expiry_cbor.to_string(),
                });
            }
            if sender != sender_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "sender".to_string(),
                    value_arg: sender.to_string(),
                    value_cbor: sender_cbor.to_string(),
                });
            }
            if canister_id != canister_id_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "canister_id".to_string(),
                    value_arg: canister_id.to_string(),
                    value_cbor: canister_id_cbor.to_string(),
                });
            }
            if method_name != method_name_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "method_name".to_string(),
                    value_arg: method_name.to_string(),
                    value_cbor: method_name_cbor,
                });
            }
            if arg != arg_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "arg".to_string(),
                    value_arg: format!("{:?}", arg),
                    value_cbor: format!("{:?}", arg_cbor),
                });
            }
        }
    }
    Ok(())
}

/// Inspect the bytes to be sent as a request_status
/// Return Ok only when the bytes can be deserialized as a request_status and all fields match with the arguments
pub fn signed_request_status_inspect(
    sender: Principal,
    request_id: &RequestId,
    ingress_expiry: u64,
    signed_request_status: Vec<u8>,
) -> Result<(), AgentError> {
    let paths: Vec<Vec<Label>> = vec![vec!["request_status".into(), request_id.to_vec().into()]];
    let envelope: Envelope<ReadStateContent> =
        serde_cbor::from_slice(&signed_request_status).map_err(AgentError::InvalidCborData)?;
    match envelope.content {
        ReadStateContent::ReadStateRequest {
            ingress_expiry: ingress_expiry_cbor,
            sender: sender_cbor,
            paths: paths_cbor,
        } => {
            if ingress_expiry != ingress_expiry_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "ingress_expiry".to_string(),
                    value_arg: ingress_expiry.to_string(),
                    value_cbor: ingress_expiry_cbor.to_string(),
                });
            }
            if sender != sender_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "sender".to_string(),
                    value_arg: sender.to_string(),
                    value_cbor: sender_cbor.to_string(),
                });
            }

            if paths != paths_cbor {
                return Err(AgentError::CallDataMismatch {
                    field: "paths".to_string(),
                    value_arg: format!("{:?}", paths),
                    value_cbor: format!("{:?}", paths_cbor),
                });
            }
        }
    }
    Ok(())
}

/// A Query Request Builder.
///
/// This makes it easier to do query calls without actually passing all arguments.
pub struct QueryBuilder<'agent> {
    agent: &'agent Agent,
    effective_canister_id: Principal,
    canister_id: Principal,
    method_name: String,
    arg: Vec<u8>,
    ingress_expiry_datetime: Option<u64>,
}

impl<'agent> QueryBuilder<'agent> {
    pub fn new(agent: &'agent Agent, canister_id: Principal, method_name: String) -> Self {
        Self {
            agent,
            effective_canister_id: canister_id,
            canister_id,
            method_name,
            arg: vec![],
            ingress_expiry_datetime: None,
        }
    }

    pub fn with_effective_canister_id(&mut self, canister_id: Principal) -> &mut Self {
        self.effective_canister_id = canister_id;
        self
    }

    pub fn with_arg<A: AsRef<[u8]>>(&mut self, arg: A) -> &mut Self {
        self.arg = arg.as_ref().to_vec();
        self
    }

    /// Takes a SystemTime converts it to a Duration by calling
    /// duration_since(UNIX_EPOCH) to learn about where in time this SystemTime lies.
    /// The Duration is converted to nanoseconds and stored in ingress_expiry_datetime
    pub fn expire_at(&mut self, time: std::time::SystemTime) -> &mut Self {
        self.ingress_expiry_datetime = Some(
            time.duration_since(std::time::UNIX_EPOCH)
                .expect("Time wrapped around")
                .as_nanos() as u64,
        );
        self
    }

    /// Takes a Duration (i.e. 30 sec/5 min 30 sec/1 h 30 min, etc.) and adds it to the
    /// Duration of the current SystemTime since the UNIX_EPOCH
    /// Subtracts a permitted drift from the sum to account for using system time and not block time.
    /// Converts the difference to nanoseconds and stores in ingress_expiry_datetime
    pub fn expire_after(&mut self, duration: std::time::Duration) -> &mut Self {
        let permitted_drift = Duration::from_secs(60);
        self.ingress_expiry_datetime = Some(
            (duration
                .as_nanos()
                .saturating_add(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("Time wrapped around")
                        .as_nanos(),
                )
                .saturating_sub(permitted_drift.as_nanos())) as u64,
        );
        self
    }

    /// Make a query call. This will return a byte vector.
    pub async fn call(&self) -> Result<Vec<u8>, AgentError> {
        self.agent
            .query_raw(
                &self.canister_id,
                self.effective_canister_id,
                self.method_name.as_str(),
                self.arg.as_slice(),
                self.ingress_expiry_datetime,
            )
            .await
    }

    /// Sign a query call. This will return a [`signed::SignedQuery`]
    /// which contains all fields of the query and the signed query in CBOR encoding
    pub fn sign(&self) -> Result<signed::SignedQuery, AgentError> {
        let request = self.agent.query_content(
            &self.canister_id,
            &self.method_name,
            &self.arg,
            self.ingress_expiry_datetime,
        )?;

        let signed_query = sign_request(&request, self.agent.identity.clone())?;
        match request {
            QueryContent::QueryRequest {
                ingress_expiry,
                sender,
                canister_id,
                method_name,
                arg,
            } => Ok(signed::SignedQuery {
                ingress_expiry,
                sender,
                canister_id,
                method_name,
                arg,
                effective_canister_id: self.effective_canister_id,
                signed_query,
            }),
        }
    }
}

pub struct UpdateCall<'agent> {
    agent: &'agent Agent,
    request_id: Pin<Box<dyn Future<Output = Result<RequestId, AgentError>> + Send + 'agent>>,
    effective_canister_id: Principal,
}
impl Future for UpdateCall<'_> {
    type Output = Result<RequestId, AgentError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.request_id.as_mut().poll(cx)
    }
}
impl UpdateCall<'_> {
    fn and_wait<'out, W>(
        self,
        waiter: W,
    ) -> Pin<Box<dyn core::future::Future<Output = Result<Vec<u8>, AgentError>> + Send + 'out>>
    where
        Self: 'out,
        W: Waiter + 'out,
    {
        async fn run<W>(_self: UpdateCall<'_>, waiter: W) -> Result<Vec<u8>, AgentError>
        where
            W: Waiter,
        {
            let request_id = _self.request_id.await?;
            _self
                .agent
                .wait(request_id, &_self.effective_canister_id, waiter)
                .await
        }
        Box::pin(run(self, waiter))
    }
}
/// An Update Request Builder.
///
/// This makes it easier to do update calls without actually passing all arguments or specifying
/// if you want to wait or not.
pub struct UpdateBuilder<'agent> {
    agent: &'agent Agent,
    pub effective_canister_id: Principal,
    pub canister_id: Principal,
    pub method_name: String,
    pub arg: Vec<u8>,
    pub ingress_expiry_datetime: Option<u64>,
}

impl<'agent> UpdateBuilder<'agent> {
    pub fn new(agent: &'agent Agent, canister_id: Principal, method_name: String) -> Self {
        Self {
            agent,
            effective_canister_id: canister_id,
            canister_id,
            method_name,
            arg: vec![],
            ingress_expiry_datetime: None,
        }
    }

    pub fn with_effective_canister_id(&mut self, canister_id: Principal) -> &mut Self {
        self.effective_canister_id = canister_id;
        self
    }

    pub fn with_arg<A: AsRef<[u8]>>(&mut self, arg: A) -> &mut Self {
        self.arg = arg.as_ref().to_vec();
        self
    }

    /// Takes a SystemTime converts it to a Duration by calling
    /// duration_since(UNIX_EPOCH) to learn about where in time this SystemTime lies.
    /// The Duration is converted to nanoseconds and stored in ingress_expiry_datetime
    pub fn expire_at(&mut self, time: std::time::SystemTime) -> &mut Self {
        self.ingress_expiry_datetime = Some(
            time.duration_since(std::time::UNIX_EPOCH)
                .expect("Time wrapped around")
                .as_nanos() as u64,
        );
        self
    }

    /// Takes a Duration (i.e. 30 sec/5 min 30 sec/1 h 30 min, etc.) and adds it to the
    /// Duration of the current SystemTime since the UNIX_EPOCH
    /// Subtracts a permitted drift from the sum to account for using system time and not block time.
    /// Converts the difference to nanoseconds and stores in ingress_expiry_datetime
    pub fn expire_after(&mut self, duration: std::time::Duration) -> &mut Self {
        let permitted_drift = Duration::from_secs(60);
        self.ingress_expiry_datetime = Some(
            (duration
                .as_nanos()
                .saturating_add(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("Time wrapped around")
                        .as_nanos(),
                )
                .saturating_sub(permitted_drift.as_nanos())) as u64,
        );
        self
    }

    /// Make an update call. This will call request_status on the RequestId in a loop and return
    /// the response as a byte vector.
    pub async fn call_and_wait<W: Waiter>(&self, waiter: W) -> Result<Vec<u8>, AgentError> {
        self.call().and_wait(waiter).await
    }

    /// Make an update call. This will return a RequestId.
    /// The RequestId should then be used for request_status (most likely in a loop).
    pub fn call(&self) -> UpdateCall {
        let request_id_future = self.agent.update_raw(
            &self.canister_id,
            self.effective_canister_id,
            self.method_name.as_str(),
            self.arg.as_slice(),
            self.ingress_expiry_datetime,
        );
        UpdateCall {
            agent: self.agent,
            request_id: Box::pin(request_id_future),
            effective_canister_id: self.effective_canister_id,
        }
    }

    /// Sign a update call. This will return a [`signed::SignedUpdate`]
    /// which contains all fields of the update and the signed update in CBOR encoding
    pub fn sign(&self) -> Result<signed::SignedUpdate, AgentError> {
        let request = self.agent.update_content(
            &self.canister_id,
            &self.method_name,
            &self.arg,
            self.ingress_expiry_datetime,
        )?;
        let signed_update = sign_request(&request, self.agent.identity.clone())?;
        let request_id = to_request_id(&request)?;
        match request {
            CallRequestContent::CallRequest {
                nonce,
                ingress_expiry,
                sender,
                canister_id,
                method_name,
                arg,
            } => Ok(signed::SignedUpdate {
                nonce,
                ingress_expiry,
                sender,
                canister_id,
                method_name,
                arg,
                effective_canister_id: self.effective_canister_id,
                signed_update,
                request_id,
            }),
        }
    }
}

/// A function to verify a certificate for a given request ID and yield the response without using async
pub fn ios_verify(
    request_id: &RequestId,
    cert: Certificate,
) -> Result<RequestStatusResponse, AgentError> {
    let sig = &cert.signature;

    let root_hash = cert.tree.digest();
    let mut msg = vec![];
    msg.extend_from_slice(IC_STATE_ROOT_DOMAIN_SEPARATOR);
    msg.extend_from_slice(&root_hash);

    fn verify(cert: &Certificate) -> Result<Vec<u8>, AgentError> {
        match &cert.delegation {
            None => Ok(IC_ROOT_KEY.to_vec()),
            Some(delegation) => {
                let cert: Certificate = serde_cbor::from_slice(&delegation.certificate)
                    .map_err(AgentError::InvalidCborData)?;
                verify(&cert)?;
                let public_key_path = [
                    "subnet".into(),
                    delegation.subnet_id.clone().into(),
                    "public_key".into(),
                ];
                lookup_value(&cert, public_key_path).map(|pk| pk.to_vec())
            }
        }
    }

    let der_key = verify(&cert)?;

    let key = extract_der(der_key)?;

    let result = bls::core_verify(sig, &*msg, &*key);
    if result != bls::BLS_OK {
        Err(AgentError::CertificateVerificationFailed())
    } else {
        lookup_request_status(cert, request_id)
    }
}
// iOS:

#[derive(Deserialize, Serialize)]
struct IOSVerify<'a> {
    request_id: RequestId,
    cert: Certificate<'a>,
}

fn error(ctx: &str, msg: String) -> *mut c_char {
    let res = json!({"stage": ctx, "error": msg}).to_string();
    CString::new(res).unwrap().into_raw()
}

#[no_mangle]
pub fn ios_verify_json(s: *const c_char) -> *mut c_char {
    let c_str = unsafe { CStr::from_ptr(s) };
    let b = c_str.to_bytes();
    let IOSVerify { request_id, cert } = match serde_json::from_slice(&b[..]) {
        Ok(v) => v,
        Err(e) => return error("Deserialization", e.to_string()),
    };
    let response = match ios_verify(&request_id, cert) {
        Ok(response) => response,
        Err(e) => return error("Verification", e.to_string()),
    };
    let json = json!({ "certified_result": response }).to_string();
    CString::new(json).unwrap().into_raw()
}

// Android:

#[cfg(feature = "jni")]
#[allow(non_snake_case)]
pub mod android {
    extern crate jni;

    use serde_json::json;

    use self::jni::objects::{JClass, JString};
    use self::jni::sys::jstring;
    use self::jni::JNIEnv;

    use super::{ios_verify, IOSVerify};

    #[no_mangle]
    pub unsafe extern "C" fn Java_co_psychedelic_plug_BLSModule_verifyJson(
        env: JNIEnv,
        _: JClass,
        s: JString,
    ) -> jstring {
        let c_str = env.get_string(s);
        match c_str {
            Ok(v) => {
                let string = v.to_str().unwrap().as_bytes();
                let IOSVerify { request_id, cert } = match serde_json::from_slice(&string[..]) {
                    Ok(v) => v,
                    Err(e) => {
                        let errorrMessage = env
                            .new_string(format!("Deserialization: {:?}", e.to_string()))
                            .unwrap()
                            .into_inner();
                        return errorrMessage;
                    }
                };
                let response = match ios_verify(&request_id, cert) {
                    Ok(response) => response,
                    Err(e) => {
                        let errorrMessage = env
                            .new_string(format!("Verification: {:?}", e.to_string()))
                            .unwrap()
                            .into_inner();
                        return errorrMessage;
                    }
                };
                let json = json!({ "certified_result": response }).to_string();
                // return env.new_string(json).unwrap().into_inner()
                return env.new_string(json).unwrap().into_inner();
            }
            Err(e) => {
                let errorrMessage = env
                    .new_string(format!("BLS Module Error: {:?}", e.to_string()))
                    .unwrap()
                    .into_inner();
                return errorrMessage;
            }
        }
    }
}

// #[test]
// fn generate_examples() {
//     fn prt(cs: CString) {
//         println!("{}", cs.into_string().unwrap());
//     }

//     let dummy = "Some Error".to_string();

//     println!("Inputs :");

//     let input = IOSVerify {
//         request_id: RequestId::new(&[7; 32]),
//         cert: Certificate {
//             tree: ic_types::hash_tree::empty(),
//             signature: vec![],
//             delegation: None,
//         },
//     };

//     println!("{}", serde_json::to_string(&input).unwrap());

//     println!("Outputs:");

//     let outputs = vec![
//         RequestStatusResponse::Unknown,
//         RequestStatusResponse::Received,
//         RequestStatusResponse::Processing,
//         RequestStatusResponse::Replied {
//             reply: Replied::CallReplied(vec![1, 2, 3, 4, 5, 6]),
//         },
//         RequestStatusResponse::Rejected {
//             reject_code: 69,
//             reject_message: "Rejected for some reason".to_string(),
//         },
//         RequestStatusResponse::Done,
//     ];

//     for response in outputs {
//         let json = json!({ "certified_result": response }).to_string();
//         println!("{}", json);
//     }

//     println!("Errors:");
//     prt(error("Deserialization", dummy.clone()));
//     prt(error("Serialization", dummy.clone()));
//     prt(error("Verification", dummy.clone()));
//     prt(error(
//         "Verification",
//         AgentError::CertificateVerificationFailed().to_string(),
//     ));
// }

// #[test]
// fn test_verify() {
//     static EXAMPLE: &str = r#"{"cert":{"tree":[1,[4,[215,185,19,205,159,218,174,17,60,50,75,242,164,38,24,109,147,224,4,57,108,110,119,82,102,4,87,100,167,52,27,226]],[1,[4,[58,72,209,252,33,61,73,48,113,3,16,79,125,114,194,181,147,14,219,168,120,123,144,99,31,52,59,58,166,138,95,10]],[2,[116,105,109,101],[3,[200,204,155,166,188,128,187,236,22]]]]],"signature":[181,117,98,29,128,49,115,147,51,44,243,157,177,223,247,83,182,12,46,212,234,175,118,90,73,53,254,117,40,235,190,185,222,71,242,80,198,207,193,217,167,242,29,24,27,89,70,141],"delegation":{"subnet_id":[46,204,41,68,123,14,239,108,36,29,207,223,125,171,7,112,147,204,214,161,38,107,224,254,156,155,18,118,2],"certificate":[217,217,247,162,100,116,114,101,101,131,1,130,4,88,32,249,66,233,213,241,228,119,64,0,9,56,178,23,20,161,166,236,138,248,194,205,64,183,150,83,29,86,196,88,116,141,134,131,1,131,2,70,115,117,98,110,101,116,131,1,131,1,131,1,130,4,88,32,53,188,32,114,102,170,31,154,27,78,234,57,62,254,145,174,51,237,76,231,112,105,237,142,136,29,134,113,106,223,123,107,131,1,130,4,88,32,102,250,154,53,163,33,4,35,115,207,22,83,65,90,14,241,211,140,160,155,225,9,234,74,203,116,233,227,233,205,234,8,131,1,130,4,88,32,54,243,205,37,125,144,251,56,228,37,151,241,147,165,224,49,219,213,133,182,41,39,147,187,4,219,71,148,128,60,224,110,131,2,88,29,46,204,41,68,123,14,239,108,36,29,207,223,125,171,7,112,147,204,214,161,38,107,224,254,156,155,18,118,2,131,1,131,2,79,99,97,110,105,115,116,101,114,95,114,97,110,103,101,115,130,3,88,27,217,217,247,129,130,74,0,0,0,0,0,224,0,0,1,1,74,0,0,0,0,0,239,255,255,1,1,131,2,74,112,117,98,108,105,99,95,107,101,121,130,3,88,133,48,129,130,48,29,6,13,43,6,1,4,1,130,220,124,5,3,1,2,1,6,12,43,6,1,4,1,130,220,124,5,3,2,1,3,97,0,145,84,28,220,123,101,196,130,130,134,201,17,96,45,148,56,222,86,73,214,152,182,15,192,106,236,115,88,147,149,208,188,167,23,70,82,78,210,255,23,178,200,218,159,188,137,127,15,7,164,11,32,72,113,182,254,150,212,94,241,11,81,209,241,213,48,208,103,154,93,184,45,233,105,41,128,95,161,124,115,121,148,235,204,35,18,210,162,91,217,71,71,236,248,243,75,130,4,88,32,36,166,67,130,186,234,41,251,110,200,37,254,141,163,106,125,233,110,187,50,15,242,37,90,177,184,222,213,234,211,27,94,130,4,88,32,220,198,118,226,168,77,241,250,232,136,55,251,99,45,93,254,230,149,233,236,237,216,110,228,200,249,148,210,91,140,16,193,131,2,68,116,105,109,101,130,3,73,163,204,248,194,139,133,167,236,22,105,115,105,103,110,97,116,117,114,101,88,48,177,153,157,46,226,201,240,67,46,40,54,208,88,167,22,200,107,201,203,56,217,210,239,35,146,172,195,229,74,248,149,249,228,79,79,202,253,35,150,175,252,161,218,179,118,254,122,67]}},"request_id":[158,54,162,163,135,135,187,174,111,106,222,181,229,251,223,43,117,85,168,120,32,143,116,174,147,14,177,102,203,186,174,206]}"#;
//     let c = CString::new(EXAMPLE).unwrap();
//     let res = ios_verify_json(c);
//     assert_eq!(
//         res,
//         CString::new(r#"{"certified_result":"Unknown"}"#).unwrap()
//     )
// }
