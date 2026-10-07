use serde_json::{Value, json};

use crate::{
    Client, Error, ErrorKind, TokenSource,
    inventory::{inventory_path, select_device, select_device_by_id, validate_action_ack},
    mutation::{Mutation, Plan},
};

/// A locator LED operation bound to one site and one device selected from inventory.
pub struct Locator<'a, T> {
    client: &'a Client<T>,
    inventory_path: String,
    device_id: String,
    device_name: Option<String>,
}

impl<T: TokenSource> Locator<'_, T> {
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn device_name(&self) -> Option<&str> {
        self.device_name.as_deref()
    }
}

impl<'a, T: TokenSource> Locator<'a, T> {
    /// Read inventory once and prepare a locator plan without sending a write.
    pub async fn plan(
        client: &'a Client<T>,
        site: &str,
        selector: &str,
        active: bool,
    ) -> Result<(Self, Plan<bool>), Error> {
        let inventory_path = inventory_path(site)?;
        let inventory = client.get(&inventory_path).await?;
        let device = select_device(&inventory, selector)?;
        let device_id = device
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| general("inventory device has no valid identifier"))?;
        if !crate::inventory::is_mac_address(device_id) {
            return Err(general("inventory device identifier is not a MAC address"));
        }
        let current = locator_state(device)?;
        let mutation = Self {
            client,
            inventory_path,
            device_id: device_id.to_owned(),
            device_name: device
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        Ok((
            mutation,
            Plan {
                current,
                desired: active,
            },
        ))
    }
}

impl<T: TokenSource> Mutation for Locator<'_, T> {
    type State = bool;

    async fn read(&self) -> Result<Self::State, Error> {
        let inventory = self.client.get(&self.inventory_path).await?;
        let device = select_device_by_id(&inventory, &self.device_id)?;
        locator_state(device)
    }

    async fn write(&self, desired: &Self::State) -> Result<(), Error> {
        let action = if *desired {
            "activateLocatorLED"
        } else {
            "deactivateLocatorLED"
        };
        let response = self
            .client
            .action(
                &self.inventory_path,
                &self.device_id,
                action,
                &json!({"id": &self.device_id}),
            )
            .await?;
        validate_action_ack(&response, &self.device_id)
    }
}

fn locator_state(device: &Value) -> Result<bool, Error> {
    match device
        .get("capabilities")
        .and_then(Value::as_object)
        .and_then(|capabilities| capabilities.get("locatorLed"))
        .and_then(Value::as_bool)
    {
        Some(true) => {}
        Some(false) => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "device does not support locator LED control",
            ));
        }
        None => return Err(general("locator LED capability is missing or unknown")),
    }
    device
        .get("isLocatorLedActive")
        .and_then(Value::as_bool)
        .ok_or_else(|| general("locator LED state is missing or unknown"))
}

fn general(message: &'static str) -> Error {
    Error::new(ErrorKind::General, message)
}
