use std::{net::SocketAddr, pin::Pin, sync::Arc, time::Instant};

use futures::{Stream, StreamExt};
use ledger_auth::JwtKeys;
use ledger_telemetry::{
    metrics as m,
    propagation::{self, HeaderExtractor},
};
use tokio_stream::wrappers::{
    BroadcastStream, TcpListenerStream, errors::BroadcastStreamRecvError,
};
use tonic::{Request, Response, Status, transport::Server};
use tracing::{Span, field::Empty};
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

const SERVICE: &str = "ledger.notifications.v1.Notifications";

fn rpc_span<T>(method: &'static str, request: &Request<T>) -> Span {
    let span = tracing::info_span!(
        "grpc",
        otel.name = %format_args!("{SERVICE}/{method}"),
        otel.kind = "server",
        otel.status_code = Empty,
        rpc.system = "grpc",
        rpc.service = SERVICE,
        rpc.method = method,
        rpc.grpc.status_code = Empty,
        user = Empty,
    );
    let headers = request.metadata().clone().into_headers();
    propagation::set_parent(&span, &HeaderExtractor(&headers));
    span
}

fn finish<T>(method: &'static str, started: Instant, result: &Result<T, Status>) {
    let code = match result {
        Ok(_) => tonic::Code::Ok,
        Err(status) => status.code(),
    };
    let span = Span::current();
    span.record("rpc.grpc.status_code", code as i32);
    if !matches!(
        code,
        tonic::Code::Ok
            | tonic::Code::InvalidArgument
            | tonic::Code::Unauthenticated
            | tonic::Code::NotFound
    ) {
        span.record("otel.status_code", "ERROR");
    }
    let labels = [("method", method.to_owned()), ("code", format!("{code:?}"))];
    metrics::counter!(m::GRPC_REQUESTS, &labels).increment(1);
    metrics::histogram!(m::GRPC_DURATION, "method" => method)
        .record(started.elapsed().as_secs_f64());
}

struct Subscriber;

impl Subscriber {
    fn open() -> Self {
        metrics::gauge!(m::NOTIFIER_SUBSCRIBERS).increment(1);
        Self
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        metrics::gauge!(m::NOTIFIER_SUBSCRIBERS).decrement(1);
    }
}

fn pushed(delivered: &crate::inbox::Delivered) {
    let span = tracing::info_span!(
        "notifier.push",
        otel.name = %format_args!("push {SERVICE}/Subscribe"),
        otel.kind = "producer",
        notification.id = %delivered.notification.id,
    );
    propagation::set_parent(&span, &delivered.trace);
    span.in_scope(|| tracing::debug!("notification pushed to subscriber"));
}

pub type NotificationStream = Pin<Box<dyn Stream<Item = Result<Notification, Status>> + Send>>;

#[tonic::async_trait]
impl Notifications for NotificationsService {
    async fn list(&self, request: Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        rpc_span("List", &request).in_scope(|| {
            let started = Instant::now();
            let result = self.list_inner(&request);
            finish("List", started, &result);
            result
        })
    }

    type SubscribeStream = NotificationStream;

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        rpc_span("Subscribe", &request).in_scope(|| {
            let started = Instant::now();
            let result = self.subscribe_inner(&request);
            finish("Subscribe", started, &result);
            result
        })
    }
}

impl NotificationsService {
    fn list_inner(&self, request: &Request<ListRequest>) -> Result<Response<ListResponse>, Status> {
        let user = self.caller(request)?;
        Span::current().record("user", user.to_string());
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

    fn subscribe_inner(
        &self,
        request: &Request<SubscribeRequest>,
    ) -> Result<Response<NotificationStream>, Status> {
        let user = self.caller(request)?;
        Span::current().record("user", user.to_string());
        let subscriber = Subscriber::open();
        let stream = BroadcastStream::new(self.inbox.subscribe()).filter_map(move |item| {
            let _open = &subscriber;
            let item = match item {
                Ok(delivered) if delivered.user_id == user => {
                    pushed(&delivered);
                    Some(Ok(delivered.notification))
                }
                Ok(_) => None,
                Err(BroadcastStreamRecvError::Lagged(missed)) => Some(Err(Status::data_loss(format!(
                    "subscriber fell behind and missed {missed} notifications, call List to resync"
                )))),
            };
            std::future::ready(item)
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
