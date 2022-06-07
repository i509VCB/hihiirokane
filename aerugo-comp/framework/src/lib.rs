pub mod client;
pub mod state;

pub mod format;
// pub mod vulkan;

use std::{error::Error, ffi::OsString, sync::Arc, time::Duration};

use smithay::{
    reexports::{
        calloop::{self, EventLoop, LoopHandle},
        wayland_server::Display,
    },
    wayland::socket::ListeningSocketSource,
};
use state::Aerugo;

use crate::client::DumbClientData;

#[derive(Debug)]
pub struct CalloopData {
    pub state: Aerugo,
    pub display: Display<Aerugo>,
}

impl CalloopData {
    // TODO: How to pass backends around?
    pub fn new(
        _loop_handle: &LoopHandle<'_, CalloopData>,
        display: Display<Aerugo>,
    ) -> Result<CalloopData, Box<dyn Error>> {
        Ok(CalloopData {
            state: Aerugo::new(&display.handle()),
            display,
        })
    }

    pub fn run(mut self, mut event_loop: EventLoop<CalloopData>) -> calloop::Result<()> {
        let signal = event_loop.get_signal();

        event_loop.run(Duration::from_millis(5), &mut self, |aerugo| {
            if !aerugo.running() {
                signal.stop();
            }

            // TODO: Poll source
            aerugo.display.dispatch_clients(&mut aerugo.state).expect("dispatch");

            // TODO: Better io error handling?
            aerugo.display.flush_clients().expect("flush");
        })
    }

    pub fn create_socket(&mut self, loop_handle: &LoopHandle<'_, CalloopData>) -> Result<OsString, Box<dyn Error>> {
        let socket = ListeningSocketSource::new_auto(None)?;
        println!("Using socket name {:?}", socket.socket_name());

        let socket_name = socket.socket_name().to_owned();

        loop_handle.insert_source(socket, |new_client, _, aerugo| {
            aerugo
                .display
                .handle()
                .insert_client(new_client, Arc::new(DumbClientData))
                .expect("handle error?");
        })?;

        Ok(socket_name)
    }

    pub fn running(&self) -> bool {
        self.state.running
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

    use crate::CalloopData;

    #[test]
    fn run_simple() {
        let event_loop = EventLoop::try_new().unwrap();
        let display = Display::new().unwrap();
        let loop_handle = event_loop.handle();

        let mut aerugo = CalloopData::new(&loop_handle, display).unwrap();
        let socket_name = aerugo.create_socket(&loop_handle).unwrap();

        // TODO: Better client spawning
        {
            Command::new("wayland-info")
                .env("WAYLAND_DISPLAY", &socket_name)
                .spawn()
                .expect("spawn");
        }

        aerugo.run(event_loop).unwrap();
    }
}