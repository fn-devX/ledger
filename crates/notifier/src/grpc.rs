use std::{net::SocketAddr, pin::Pin, sync::Arc};

use futures::{Stream, StreamExt};
use ledger_auth::JwtKeys;
use tokio_stream::wrappers::{
    BroadcastStream, TcpListenerStream, errors::BroadcastStreamRecvError,
};
use tonic::{Request, Response, Status, transport::Server};
use uuid::Uuid;

use crate::{
    inbox::Inbox,
    proto::{
        FILE_DESCRIPTOR_SET, ListRequest, ListResponse, Notification, SubscribeRequest,
        notifications_server::{Notifications, NotificationsServer},
    },
};

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;

#[derive(Debug, Clone)]
pub struct NotificationsService {
    inbox: Arc<Inbox>,
    jwt: Arc<JwtKeys>,
}

impl NotificationsService {
    pub fn new(inbox: Arc<Inbox>, jwt: Arc<JwtKeys>) -> Self {
        Self { inbox, jwt }
    }

    fn caller<T>(&self, request: &Request<T>) -> Result<Uuid, Status> {
        let header = request
            .metadata()
            .get("authorization")
            .ok_or_else(|| Status::unauthenticated("missing bearer token"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("malformed authorization header"))?;
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .ok_or_else(|| Status::unauthenticated("expected `authorization: Bearer <token>`"))?;
        self.jwt
            .verify(token.trim())
            .map(|subject| subject.user_id.as_uuid())
            .map_err(|e| Status::unauthenticated(e.to_string()))
    }
}

pub type NotificationStream = Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send>>;

#[tonic::async_trait]
impl Notifications for NotificationsService {
    async fn list(&self, request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let user = self.caller(&request)?;
        let limit = match request.get_ref().limit {
            0 => DEFAULT_LIMIT,
            n if n > MAX_LIMIT => {
                return Err(Status::invalid_argument(format!(
                    "limit must be at most {MAX_LIMIT}"
                )));
            }
            n => n,
        };
        Ok(Response::new(ListResponse {
            notifications: self.inbox.list(user, limit as usize),
        }))
    }

    type SubscribeStream = NotificationStream;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let user = self.caller(&request)?;
        let stream = BroadcastStream::new(self.inbox.subscribe()).filter_map(move |item| async move {
            match item {
                Ok(delivered) if delivered.user_id == user => Some(Ok(delivered.notification)),
                Ok(_) => None,
                Err(BroadcastStreamRecvError::Lagged(missed)) => Some(Err(Status::data_loss(format!(
                    "subscriber fell behind and missed {missed} notifications, call List to resync"
                )))),
            }
        });
        Ok(Response::new(Box::pin(stream)))
    }
}

fn routes(
    service: NotificationsService,
) -> Result<tonic::service::Routes, Box<dyn std::error::Error + Send + Sync>> {
    let reflection = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
        .build_v1()?;
    Ok(tonic::service::Routes::new(NotificationsServer::new(service)).add_service(reflection))
}

pub async fn serve(
    addr: SocketAddr,
    service: NotificationsService,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Server::builder()
        .add_routes(routes(service)?)
        .serve_with_shutdown(addr, shutdown)
        .await?;
    Ok(())
}

pub async fn serve_on(
    listener: tokio::net::TcpListener,
    service: NotificationsService,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Server::builder()
        .add_routes(routes(service)?)
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await?;
    Ok(())
}
