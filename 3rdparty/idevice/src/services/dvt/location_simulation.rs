//! Location Simulation service client for iOS instruments protocol.
//!
//! This module abstracts simulating the device's location over
//! the remote server protocol. Note that a connection must be
//! maintained to keep location simulated.
//!
//! # Example
//! ```rust,no_run
//! #[tokio::main]
//! async fn main() -> Result<(), IdeviceError> {
//!     // Create base client (implementation specific)
//!     let mut client = RemoteServerClient::new(your_transport);
//!
//!     // Create process control client
//!     let mut process_control = ProcessControlClient::new(&mut client).await?;
//!
//!     // Launch an app
//!     let pid = process_control.launch_app(
//!         "com.example.app",
//!         None,       // Environment variables
//!         None,       // Arguments
//!         false,      // Start suspended
//!         true        // Kill existing
//!     ).await?;
//!     println!("Launched app with PID: {}", pid);
//!
//!     // Disable memory limits
//!     process_control.disable_memory_limit(pid).await?;
//!
//!     // Kill the app
//!     process_control.kill_app(pid).await?;
//!
//!     Ok(())
//! }
//! ```

use plist::Value;

use crate::{
    IdeviceError, ReadWrite,
    dvt::{
        message::AuxValue,
        remote_server::{Channel, RemoteServerClient},
    },
    obf,
};

/// A client for the location simulation service
#[derive(Debug)]
pub struct LocationSimulationClient<'a, R: ReadWrite> {
    /// The underlying channel used for communication
    channel: Channel<'a, R>,
}

impl<'a, R: ReadWrite> LocationSimulationClient<'a, R> {
    /// Opens a new channel on the remote server client for location simulation
    ///
    /// # Arguments
    /// * `client` - The remote server client to connect with
    ///
    /// # Returns
    /// The client on success, IdeviceError on failure
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!(
                "com.apple.instruments.server.services.LocationSimulation"
            ))
            .await?; // Drop `&mut client` before continuing

        Ok(Self { channel })
    }

    /// Clears the set GPS location
    pub async fn clear(&mut self) -> Result<(), IdeviceError> {
        let method = Value::String("stopLocationSimulation".into());

        self.channel.call_method(Some(method), None, true).await?;

        let _ = self.channel.read_message().await?;

        Ok(())
    }

    /// Sets the GPS location
    ///
    /// # Arguments
    /// * `latitude` - The f64 latitude value
    /// * `longitude` - The f64 longitude value
    ///
    /// # Errors
    /// Returns an IdeviceError on failure
    pub async fn set(&mut self, latitude: f64, longitude: f64) -> Result<(), IdeviceError> {
        let method = Value::String("simulateLocationWithLatitude:longitude:".into());

        self.channel
            .call_method(
                Some(method),
                Some(vec![
                    AuxValue::archived_value(latitude),
                    AuxValue::archived_value(longitude),
                ]),
                true,
            )
            .await?;

        // We don't actually care what's in the response, but we need to request one and read it
        let _ = self.channel.read_message().await?;

        Ok(())
    }
}
