use std::str;

use anyhow::{anyhow, bail, ensure, Result};
use futures::Future;
use shared_crypto::intent::Intent;
use sui_keys::keystore::{AccountKeystore, Keystore};
use sui_sdk::{
    rpc_types::{
        Page, SuiExecutionStatus, SuiObjectDataOptions, SuiTransactionBlockEffects,
        SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponse,
        SuiTransactionBlockResponseOptions,
    },
    SuiClient,
};
use sui_types::{
    base_types::{ObjectID, ObjectRef, SuiAddress},
    object::Owner,
    quorum_driver_types::ExecuteTransactionRequestType,
    transaction::{CallArg, ProgrammableTransaction, Transaction, TransactionData},
};

pub async fn sign_and_send_ptb(
    client: &SuiClient,
    keystore: &Keystore,
    address: SuiAddress,
    programmable_transaction: ProgrammableTransaction,
    gas_coin: ObjectRef,
    gas_budget: u64,
) -> Result<SuiTransactionBlockResponse> {
    let gas_price = client.read_api().get_reference_gas_price().await?;

    let transaction = TransactionData::new_programmable(
        address,
        vec![gas_coin],
        programmable_transaction,
        gas_budget,
        gas_price,
    );
    let signature = keystore.sign_secure(&address, &transaction, Intent::sui_transaction())?;
    let response = client
        .quorum_driver_api()
        .execute_transaction_block(
            Transaction::from_data(transaction, vec![signature]),
            SuiTransactionBlockResponseOptions::full_content(),
            Some(ExecuteTransactionRequestType::WaitForLocalExecution),
        )
        .await?;
    ensure!(
        response.confirmed_local_execution == Some(true),
        "Transaction execution was not confirmed"
    );
    match response
        .effects
        .as_ref()
        .ok_or_else(|| anyhow!("No transaction effects in response"))?
        .status()
    {
        SuiExecutionStatus::Success => Ok(response),
        SuiExecutionStatus::Failure { error } => {
            Err(anyhow!("Error in transaction execution: {}", error))
        }
    }
}

pub async fn get_object_ref_from_id(client: &SuiClient, id: ObjectID) -> Result<ObjectRef> {
    client
        .read_api()
        .get_object_with_options(id, SuiObjectDataOptions::new())
        .await?
        .object_ref_if_exists()
        .ok_or_else(|| anyhow!("Could not get object reference for object with id {}", id))
}

pub async fn call_arg_from_shared_object_id(
    client: &SuiClient,
    id: ObjectID,
    mutable: bool,
) -> Result<CallArg> {
    let Some(Owner::Shared {
        initial_shared_version,
    }) = client
        .read_api()
        .get_object_with_options(id, SuiObjectDataOptions::new().with_owner())
        .await?
        .owner()
    else {
        bail!("Trying to get the initial version of a non-shared object")
    };
    Ok(CallArg::Object(
        sui_types::transaction::ObjectArg::SharedObject {
            id,
            initial_shared_version,
            mutable,
        },
    ))
}

pub async fn handle_pagination<F, T, C, Fut>(
    closure: F,
) -> Result<impl Iterator<Item = T>, sui_sdk::error::Error>
where
    F: FnMut(Option<C>) -> Fut,
    T: 'static,
    Fut: Future<Output = Result<Page<T, C>, sui_sdk::error::Error>>,
{
    handle_pagination_with_cursor(closure, None).await
}

pub(crate) async fn handle_pagination_with_cursor<F, T, C, Fut>(
    mut closure: F,
    mut cursor: Option<C>,
) -> Result<impl Iterator<Item = T>, sui_sdk::error::Error>
where
    F: FnMut(Option<C>) -> Fut,
    T: 'static,
    Fut: Future<Output = Result<Page<T, C>, sui_sdk::error::Error>>,
{
    let mut cont = true;
    let mut iterators = vec![];
    while cont {
        let page = closure(cursor).await?;
        cont = page.has_next_page;
        cursor = page.next_cursor;
        iterators.push(page.data.into_iter());
    }
    Ok(iterators.into_iter().flatten())
}

/// Convert the hex representation of an object id to base36
pub fn id_to_base36(id: &ObjectID) -> Result<String> {
    const BASE36: &[u8] = "0123456789abcdefghijklmnopqrstuvwxyz".as_bytes();
    let source = id.into_bytes();
    let base = BASE36.len();
    let size = source.len() * 2;
    let mut encoding = vec![0; size];
    let mut high = size - 1;
    for digit in &source {
        let mut carry = *digit as usize;
        let mut it = size - 1;
        while it > high || carry != 0 {
            carry += 256 * encoding[it];
            encoding[it] = carry % base;
            carry /= base;
            it -= 1;
        }
        high = it;
    }
    let skip = encoding.iter().take_while(|v| **v == 0).count();
    let string = str::from_utf8(
        &(encoding[skip..]
            .iter()
            .map(|&c| BASE36[c])
            .collect::<Vec<_>>()),
    )
    .unwrap()
    .to_owned();
    Ok(string)
}

/// Get the object id of the site that was published in the transaction
pub fn get_site_id_from_response(
    address: SuiAddress,
    effects: &SuiTransactionBlockEffects,
) -> Result<ObjectID> {
    Ok(effects
        .created()
        .iter()
        .find(|c| c.owner == address)
        .expect("Could not find the object ID for the created blocksite.")
        .reference
        .object_id)
}

pub async fn get_dynamic_field_names(client: &SuiClient, object: ObjectID) -> Result<Vec<String>> {
    handle_pagination(|cursor| client.read_api().get_dynamic_fields(object, cursor, None))
        .await?
        .map(|d| {
            d.name
                .value
                .as_str()
                .map(|s| s.to_owned())
                .ok_or(anyhow!("Could not read the name of the dynamic field"))
        })
        .collect()
}

#[cfg(test)]
mod test_util {
    use sui_types::base_types::ObjectID;

    use super::*;

    #[test]
    fn test_id_to_base36() {
        let id = ObjectID::from_hex_literal(
            "0x05fb8843a23017cbf1c907bd559a2d6191b77bc595d4c83853cca14cc784c0a8",
        )
        .unwrap();
        let converted = id_to_base36(&id).unwrap();
        assert_eq!(
            &converted,
            "5d8t4gd5q8x4xcfyctpygyr5pnk85x54o7ndeq2j4pg9l7rmw"
        );
    }
}
