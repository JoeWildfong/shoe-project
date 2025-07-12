use core::prelude::rust_2024::*;
use defmt::{info, warn};
use trouble_host::prelude::*;
use uuid::uuid;

use crate::read_battery;

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;

/// Max number of L2CAP channels.
const L2CAP_CHANNELS_MAX: usize = 2; // Signal + att

// Max transfer unit for a single L2CAP packet
const L2CAP_MTU: usize = 255;

// GATT Server definition
#[gatt_server]
struct Server {
    tighten_service: TightenService,
    battery_service: BatteryService,
}

const TIGHTEN_SERVICE: [u8; 16] = uuid!("1953e703-82d4-4142-9efe-30a87538c7de")
    .as_u128()
    .to_le_bytes();
const TIGHTEN_CHARACTERISTIC: [u8; 16] = uuid!("45803b5a-8847-465b-9d4a-0af234a0db11")
    .as_u128()
    .to_le_bytes();

#[gatt_service(uuid = TIGHTEN_SERVICE)]
struct TightenService {
    /// Tightening amount (-1 = loosening, 0 = stop, 1 = tightening)
    #[characteristic(uuid = TIGHTEN_CHARACTERISTIC, write, notify)]
    tightening: i8,
}

#[gatt_service(uuid = bt_hci::uuid::service::BATTERY)]
struct BatteryService {
    #[characteristic(uuid = bt_hci::uuid::characteristic::BATTERY_LEVEL, read)]
    level: u8,
}

pub async fn run_ble(controller: impl Controller, mut on_command: impl FnMut(i8)) {
    // Using a fixed "random" address can be useful for testing. In real scenarios, one would
    // use e.g. the MAC 6 byte array as the address (how to get that varies by the platform).
    let address: Address = Address::random([0xff, 0x8f, 0x1a, 0x05, 0xe4, 0xff]);
    info!("Our address = {:?}", address);

    let mut resources: HostResources<CONNECTIONS_MAX, L2CAP_CHANNELS_MAX, L2CAP_MTU> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources).set_random_address(address);
    let Host {
        mut peripheral,
        runner,
        ..
    } = stack.build();

    info!("Starting advertising and GATT service");
    let server = Server::new_with_config(GapConfig::Peripheral(PeripheralConfig {
        name: "Shoe",
        appearance: &appearance::running_walking_sensor::ON_SHOE_RUNNING_WALKING_SENSOR,
    }))
    .unwrap();

    let _ = embassy_futures::join::join(ble_task(runner), async {
        loop {
            match advertise("Left Shoe", &mut peripheral, &server).await {
                Ok(conn) => {
                    gatt_events_task(&server, &conn, &mut on_command).await.ok();
                }
                Err(e) => {
                    let e = defmt::Debug2Format(&e);
                    panic!("[adv] error: {:?}", e);
                }
            }
        }
    })
    .await;
}

async fn ble_task<C: Controller>(mut runner: Runner<'_, C>) {
    loop {
        if let Err(e) = runner.run().await {
            let e = defmt::Debug2Format(&e);
            panic!("[ble_task] error: {:?}", e);
        }
    }
}

async fn gatt_events_task(
    server: &Server<'_>,
    conn: &GattConnection<'_, '_>,
    mut on_command: impl FnMut(i8),
) -> Result<(), Error> {
    let tightening = server.tighten_service.tightening;
    let battery_level = server.battery_service.level;
    let reason = loop {
        match conn.next().await {
            GattConnectionEvent::Disconnected { reason } => break reason,
            GattConnectionEvent::Gatt { event: Err(e) } => {
                warn!("[gatt] error processing event: {:?}", e)
            }
            GattConnectionEvent::Gatt { event: Ok(event) } => {
                match &event {
                    GattEvent::Read(event) => {
                        if event.handle() == tightening.handle {
                            let value = server.get(&tightening);
                            info!(
                                "[gatt] Read Event to Tightening Characteristic: {:?}",
                                value
                            );
                        } else if event.handle() == battery_level.handle {
                            let value = read_battery();
                            server.set(&battery_level, &value).unwrap();
                            info!(
                                "[gatt] Read Event to Battery Level Characteristic: {:?}",
                                value
                            );
                        }
                    }
                    GattEvent::Write(event) => {
                        if event.handle() == tightening.handle {
                            info!(
                                "[gatt] Write Event to Level Characteristic: {:?}",
                                event.data()
                            );
                            if let Ok(v) = event.value(&tightening) {
                                on_command(v);
                            }
                        }
                    }
                };
                // This step is also performed at drop(), but writing it explicitly is necessary
                // in order to ensure reply is sent.
                match event.accept() {
                    Ok(reply) => reply.send().await,
                    Err(e) => warn!("[gatt] error sending response: {:?}", e),
                };
            }
            _ => {} // ignore other Gatt Connection Events
        }
    };
    info!("[gatt] disconnected: {:?}", reason);
    info!("[gatt] stopping motor due to disconnect");
    on_command(0);
    Ok(())
}

/// Create an advertiser to use to connect to a BLE Central, and wait for it to connect.
async fn advertise<'values, 'server, C: Controller>(
    name: &'values str,
    peripheral: &mut Peripheral<'values, C>,
    server: &'server Server<'values>,
) -> Result<GattConnection<'values, 'server>, BleHostError<C::Error>> {
    let mut advertiser_data = [0; 47];
    let len = AdStructure::encode_slice(
        &[
            AdStructure::Flags(LE_GENERAL_DISCOVERABLE | BR_EDR_NOT_SUPPORTED),
            AdStructure::ServiceUuids128(&[TIGHTEN_SERVICE]),
            AdStructure::ServiceUuids16(&[bt_hci::uuid::service::BATTERY.to_le_bytes()]),
            AdStructure::CompleteLocalName(name.as_bytes()),
        ],
        &mut advertiser_data[..],
    )?;
    let advertiser = peripheral
        .advertise(
            &Default::default(),
            Advertisement::ConnectableScannableUndirected {
                adv_data: &advertiser_data[..len],
                scan_data: &[],
            },
        )
        .await?;
    info!("[adv] advertising");
    let conn = advertiser.accept().await?.with_attribute_server(server)?;
    info!("[adv] connection established");
    Ok(conn)
}
