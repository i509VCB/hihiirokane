use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};

pub struct DumbClientData;

impl ClientData for DumbClientData {
    fn initialized(&self, _: ClientId) {}

    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        println!("{:?}: {:?}", client_id, reason);
    }
}