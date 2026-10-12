pub mod grpc;
pub mod inbox;
pub mod projection;

#[allow(clippy::all, clippy::pedantic)]
pub mod proto {
    tonic::include_proto!("ledger.notifications.v1");

    pub const FILE_DESCRIPTOR_SET: &[u8] =
        tonic::include_file_descriptor_set!("notifications_descriptor");
}
