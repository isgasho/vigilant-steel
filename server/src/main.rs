//! Entrypoint and eventloop for server.

use game::Game;
use game::net::udp::UdpServer;
use log::{info, warn};
use std::thread::sleep;
use std::time::{Duration, SystemTime};

const TIME_STEP: f32 = 0.080;

fn to_secs(dt: Duration) -> f32 {
    dt.as_secs() as f32 + dt.subsec_nanos() as f32 * 0.000_000_001
}

/// Entrypoint for server.
fn main() {
    color_logger::init(log::Level::Info).unwrap();
    info!("Starting up");

    let mut game = Game::new_server(UdpServer::new(34244));

    let mut previous = SystemTime::now();
    let mut timer = 0.0;

    loop {
        let now = SystemTime::now();

        match now.duration_since(previous) {
            Ok(dt) => {
                let dt = to_secs(dt);
                if dt > 0.5 {
                    warn!("Clock jumped forward by {} seconds!", dt);
                    timer = 5.0 * TIME_STEP;
                } else {
                    timer += dt;
                }
                while timer > TIME_STEP {
                    game.update(TIME_STEP);
                    timer -= TIME_STEP;
                }

                if TIME_STEP - timer > 0.001 {
                    sleep(Duration::new(
                        0,
                        ((TIME_STEP - timer) * 1_000_000_000.0) as u32,
                    ));
                }
            }
            Err(e) => {
                warn!(
                    "Clock jumped backward by {} seconds!",
                    to_secs(e.duration())
                );
                timer = 0.0;
            }
        }

        previous = now;
    }
}
